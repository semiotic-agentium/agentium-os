// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host-managed Slack event producer registration.
//!
//! This operationalizes `support/slack` as an event source without hard-coding
//! Slack-specific registration in the runner.

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{
    AgentDispatchRoutingKey, BamlRtError, EventSchemaVersion, EventSourceKind, IngressStore,
    ProducedEvent, Result, clock_events, host_source_records_schema_version,
    host_wire::wire,
    ingress_store::{IngressId, IngressItem},
};
use baml_rt_tools::{
    EventProducer, EventProducerBuildContext, EventProducerBuildFuture, EventProducerProvider,
    ProducerCheckpoint, ProducerPoll,
};
use integrations_slack_read::SlackReadClient;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::channel::SlackChannelSelector;

/// Generic raw ingress schema for host-managed source records.
pub const RAW_SOURCE_SCHEMA_VERSION: &str = wire::HOST_SOURCE_RECORDS_V1;
/// Generic intake routing key for raw source ingress.
pub const RAW_SOURCE_ROUTING_KEY: &str = wire::SOURCE_RECORDS_ROUTING_KEY;

/// Transport mode for Slack event ingestion.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlackTransportConfig {
    /// REST channel polling (owned by task-daemon). Runner uses inbox drain only.
    #[default]
    Polling,
    /// Connect via Slack Socket Mode WebSocket. Requires SLACK_APP_TOKEN.
    SocketMode,
}

/// Config for host-managed Slack source ingestion.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType, Default)]
#[serde(deny_unknown_fields)]
pub struct SlackEventProducerConfig {
    #[baml(
        description = "Slack channels to ingest for raw host event delivery. Entries may be channel names like `agentium-eng` or channel IDs like `C123ABC456`."
    )]
    #[serde(default)]
    pub channels: Vec<String>,
    /// Transport mode. Defaults to polling when absent.
    #[serde(default)]
    #[baml(skip)]
    pub transport: SlackTransportConfig,
}

impl SlackEventProducerConfig {
    fn normalized_channels(&self) -> Result<Vec<SlackChannelSelector>> {
        let mut seen = HashSet::new();
        let mut channels = Vec::new();

        for raw in &self.channels {
            let channel = SlackChannelSelector::parse(raw)?;
            if seen.insert(channel.clone()) {
                channels.push(channel);
            }
        }

        Ok(channels)
    }
}

/// Serialized into the opaque [`ProducerCheckpoint`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SlackInboxProducerCheckpoint {
    #[serde(default)]
    delivered_ingress_ids: Vec<IngressId>,
}

impl SlackInboxProducerCheckpoint {
    fn from_checkpoint(checkpoint: &ProducerCheckpoint) -> Self {
        match checkpoint.value() {
            Some(raw) => serde_json::from_str(raw).unwrap_or_else(|err| {
                warn!(
                    error = %err,
                    "corrupt support/slack inbox checkpoint; resetting delivery reconciliation state"
                );
                Self::default()
            }),
            None => Self::default(),
        }
    }

    fn to_checkpoint(&self) -> ProducerCheckpoint {
        match serde_json::to_string(self) {
            Ok(value) => ProducerCheckpoint::some(value),
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "failed to serialize support/slack inbox checkpoint; cursor will not advance"
                );
                ProducerCheckpoint::none()
            }
        }
    }
}

/// Maximum persisted Slack ingress items emitted by the inbox producer per poll.
const MAX_SLACK_INBOX_ITEMS_PER_POLL: usize = 100;
/// Stable producer identity for the durable Slack inbox.
const SLACK_INBOX_PRODUCER_KEY: &str = "support/slack:inbox";
/// Wait one minute before retrying an emitted-but-unconfirmed durable inbox item.
const SLACK_INGRESS_RETRY_AFTER_MS: u64 = 60_000;

pub struct SlackInboxEventProducer {
    store: Arc<dyn IngressStore>,
    producer_key: &'static str,
    source_kind: EventSourceKind,
    source_kinds: Vec<EventSourceKind>,
}

impl SlackInboxEventProducer {
    fn new(store: Arc<dyn IngressStore>) -> Result<Self> {
        let (_, _, source_kind) = raw_source_dispatch_contract()?;
        Ok(Self {
            store,
            producer_key: SLACK_INBOX_PRODUCER_KEY,
            source_kinds: vec![source_kind.clone()],
            source_kind,
        })
    }

    fn ingress_item_to_event(&self, item: &IngressItem) -> Result<ProducedEvent> {
        let payload: serde_json::Value =
            serde_json::from_str(&item.payload_json).map_err(|err| {
                BamlRtError::InvalidArgument(format!(
                    "failed to deserialize stored ingress payload: {err}"
                ))
            })?;
        ProducedEvent::host_source_records(
            self.source_kind.clone(),
            item.source_key.clone(),
            payload,
            Some(item.ingress_id.to_string()),
            None,
        )
    }
}

fn raw_source_dispatch_contract()
-> Result<(AgentDispatchRoutingKey, EventSchemaVersion, EventSourceKind)> {
    let routing_key =
        AgentDispatchRoutingKey::parse(wire::SOURCE_RECORDS_ROUTING_KEY).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "invalid static Slack routing key '{routing_key}'",
                routing_key = wire::SOURCE_RECORDS_ROUTING_KEY
            ))
        })?;
    let schema_version = host_source_records_schema_version();
    let source_kind = slack_source_kind()?;
    Ok((routing_key, schema_version, source_kind))
}

fn slack_source_kind() -> Result<EventSourceKind> {
    EventSourceKind::parse("slack").ok_or_else(|| {
        BamlRtError::InvalidArgument("invalid static Slack source kind 'slack'".to_string())
    })
}

#[async_trait]
impl EventProducer for SlackInboxEventProducer {
    fn producer_key(&self) -> &str {
        self.producer_key
    }

    fn source_kinds(&self) -> &[EventSourceKind] {
        &self.source_kinds
    }

    async fn poll(&self, checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll> {
        let store = self.store.as_ref();
        let now_unix_ms = baml_rt_core::now_unix_ms(clock_events::SLACK_INGRESS);
        let checkpoint_state = SlackInboxProducerCheckpoint::from_checkpoint(checkpoint);
        let reconciled_deliveries = !checkpoint_state.delivered_ingress_ids.is_empty();
        if reconciled_deliveries {
            store
                .mark_delivered(&checkpoint_state.delivered_ingress_ids, now_unix_ms)
                .await?;
            debug!(
                delivered_count = checkpoint_state.delivered_ingress_ids.len(),
                "support/slack inbox reconciled delivered ingress items from persisted checkpoint"
            );
        }

        let reclaimed = store
            .requeue_stale(now_unix_ms.saturating_sub(SLACK_INGRESS_RETRY_AFTER_MS))
            .await?;
        if reclaimed > 0 {
            warn!(
                reclaimed_count = reclaimed,
                retry_after_ms = SLACK_INGRESS_RETRY_AFTER_MS,
                "support/slack inbox reclaimed stale emitted ingress items after delivery timeout"
            );
        }

        let pending_items = store
            .list_pending(&self.source_kinds, MAX_SLACK_INBOX_ITEMS_PER_POLL)
            .await?;
        let pending_ids = pending_items
            .iter()
            .map(|item| item.ingress_id.clone())
            .collect::<Vec<_>>();
        let emitted_ids = store.mark_emitted(&pending_ids, now_unix_ms).await?;
        let emitted_id_set = emitted_ids
            .iter()
            .map(IngressId::as_str)
            .collect::<HashSet<_>>();
        let pending_items = pending_items
            .into_iter()
            .filter(|item| emitted_id_set.contains(item.ingress_id.as_str()))
            .collect::<Vec<_>>();
        if emitted_ids.len() < pending_ids.len() {
            debug!(
                requested_count = pending_ids.len(),
                emitted_count = emitted_ids.len(),
                "support/slack inbox skipped rows that were no longer eligible for emission claim"
            );
        }

        let mut delivered_ingress_ids = Vec::with_capacity(pending_items.len());
        let mut events = Vec::with_capacity(pending_items.len());
        for item in &pending_items {
            let ingress_id = item.ingress_id.clone();
            let source_key = item.source_key.clone();
            let event = self.ingress_item_to_event(item).map_err(|err| {
                BamlRtError::InvalidArgument(format!(
                    "support/slack inbox failed to convert ingress item '{ingress_id}' from '{source_key}': {err}"
                ))
            })?;
            delivered_ingress_ids.push(ingress_id);
            events.push(event);
        }
        let checkpoint = if events.is_empty() && delivered_ingress_ids.is_empty() {
            if reconciled_deliveries {
                SlackInboxProducerCheckpoint::default().to_checkpoint()
            } else {
                ProducerCheckpoint::none()
            }
        } else {
            SlackInboxProducerCheckpoint {
                delivered_ingress_ids,
            }
            .to_checkpoint()
        };

        Ok(ProducerPoll { events, checkpoint })
    }
}

/// Build all configured Slack event producer instances from tool config.
pub fn build_slack_event_producers(ctx: EventProducerBuildContext) -> EventProducerBuildFuture {
    Box::pin(async move {
        let Some(store) = ctx.ingress_store else {
            return Ok(vec![]);
        };
        let config = match ctx.config {
            Some(value) => {
                serde_json::from_value::<SlackEventProducerConfig>(value).map_err(|err| {
                    BamlRtError::InvalidArgument(format!(
                        "invalid config for {} event producers: {err}",
                        ctx.metadata.name
                    ))
                })?
            }
            None => SlackEventProducerConfig::default(),
        };

        let channels = config.normalized_channels()?;

        match config.transport {
            SlackTransportConfig::SocketMode => {
                if channels.is_empty() {
                    return Err(BamlRtError::InvalidArgument(
                        "Socket Mode transport requires at least one channel".to_string(),
                    ));
                }
                let store = store.clone();
                let client = SlackReadClient::new();
                let app_token = client.auth().app_token.clone().ok_or_else(|| {
                    BamlRtError::InvalidArgument(
                        "Socket Mode transport requires SLACK_APP_TOKEN (xapp-...)".to_string(),
                    )
                })?;

                // TODO: the cancel token is currently inert — EventProducerBuildFuture
                // returns only `Vec<Arc<dyn EventProducer>>` so there is no way to
                // hand the token back to the runner, and the runner uses
                // JoinHandle::abort() for shutdown. The receiver exits correctly via
                // task abort. Wire this up when EventProducer gains shutdown support.
                let cancel = tokio_util::sync::CancellationToken::new();
                let receiver_handle = crate::socket_mode::start_socket_mode_receiver(
                    client,
                    app_token,
                    channels,
                    store.clone(),
                    cancel,
                )
                .await?;
                tokio::spawn(async move {
                    match receiver_handle.await {
                        Ok(()) => warn!("Socket Mode receiver task exited unexpectedly"),
                        Err(err) => warn!(error = %err, "Socket Mode receiver task panicked"),
                    }
                });

                let producers: Vec<Arc<dyn EventProducer>> =
                    vec![Arc::new(SlackInboxEventProducer::new(store.clone())?)
                        as Arc<dyn EventProducer>];
                Ok(producers)
            }
            SlackTransportConfig::Polling => {
                // Channel polling is owned by task-daemon (`SlackTaskSource`). The runner
                // only drains push-ingress events (e.g. Socket Mode) via the inbox producer.
                tracing::info!(
                    tool = %ctx.metadata.name,
                    channel_count = channels.len(),
                    "Slack REST polling is owned by task-daemon; runner registers inbox drain only"
                );
                Ok(vec![
                    Arc::new(SlackInboxEventProducer::new(store)?) as Arc<dyn EventProducer>
                ])
            }
        }
    })
}

inventory::submit! {
    EventProducerProvider {
        tool_name: "support/slack",
        build: build_slack_event_producers,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use axum::{Json, Router, extract::Query, routing::get};
    use baml_rt_core::IngressStore;
    use baml_rt_tools::{
        ProducerCheckpoint, ingress_store::test_support::install_memory_ingress_store,
    };
    use serde_json::json;
    use test_support::common::TempEnvVar;

    use super::*;
    use crate::{channel::SlackChannelSelector, normalize::normalize_polling_batch};

    #[derive(Clone, Default)]
    struct MockState {
        hits: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    impl MockState {
        async fn push_hit(&self, hit: String) {
            self.hits.lock().await.push(hit);
        }

        async fn snapshot(&self) -> Vec<String> {
            self.hits.lock().await.clone()
        }
    }

    async fn start_mock_server(app: Router) -> std::io::Result<String> {
        let (listener, addr) = test_support::common::bind_ephemeral_tokio("127.0.0.1").await?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(format!("http://{addr}/api"))
    }

    #[test]
    fn config_rejects_blank_channels() {
        let config = SlackEventProducerConfig {
            channels: vec!["   ".into()],
            ..Default::default()
        };
        let err = config.normalized_channels().unwrap_err().to_string();
        assert!(err.contains("must not be empty"));
    }

    #[test]
    fn config_treats_uppercase_channel_names_without_digits_as_names() {
        let config = SlackEventProducerConfig {
            channels: vec!["CLUBROOMS".into()],
            ..Default::default()
        };
        assert_eq!(
            config.normalized_channels().unwrap(),
            vec![SlackChannelSelector::ChannelName("clubrooms".to_string())]
        );
    }

    #[tokio::test]
    async fn inbox_producer_reconciles_delivery_on_the_next_poll() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let (_store_guard, store) = install_memory_ingress_store();
        let source_key =
            crate::channel::slack_polling_source_key("C123ABC456").expect("valid source key");
        let batch_messages = [json!({
            "type": "message",
            "user": "U123",
            "text": "Track the OAuth docs follow-up.",
            "ts": "1700000001.000001",
            "thread_ts": "1700000001.000001"
        })];
        let normalized_batch = normalize_polling_batch(
            RAW_SOURCE_SCHEMA_VERSION,
            "C123ABC456",
            &source_key,
            "C123ABC456",
            &batch_messages,
            1_700_000_100,
        );
        let ingress_item = IngressItem {
            ingress_id: IngressId::parse("support/slack:test-inbox-001").expect("valid ingress id"),
            source_kind: slack_source_kind().expect("valid slack source kind"),
            source_key: source_key.clone(),
            payload_json: serde_json::to_string(&normalized_batch).expect("serialize batch"),
            enqueued_at_unix_ms: 1_700_000_100_000,
        };
        store
            .enqueue(&ingress_item)
            .await
            .expect("enqueue sample durable ingress item");

        let producer = SlackInboxEventProducer::new(store.clone()).expect("build inbox producer");
        let first_poll = producer
            .poll(&ProducerCheckpoint::none())
            .await
            .expect("first inbox poll should emit the durable item");
        assert_eq!(first_poll.events.len(), 1);
        assert_eq!(
            first_poll.events[0].message_id.as_deref(),
            Some(ingress_item.ingress_id.as_str())
        );
        assert_eq!(
            first_poll.events[0].source_key.as_str(),
            ingress_item.source_key.as_str()
        );
        let first_checkpoint =
            SlackInboxProducerCheckpoint::from_checkpoint(&first_poll.checkpoint);
        assert_eq!(
            first_checkpoint.delivered_ingress_ids,
            vec![ingress_item.ingress_id.clone()]
        );
        assert_eq!(
            store.pending_items().await.len(),
            1,
            "delivery should not be reconciled until the next poll checkpoint round"
        );

        let second_poll = producer
            .poll(&first_poll.checkpoint)
            .await
            .expect("second inbox poll should reconcile the delivered item");
        assert!(
            second_poll.events.is_empty(),
            "after reconciliation there should be nothing left to emit"
        );
        assert!(
            store.pending_items().await.is_empty(),
            "reconciled inbox item should no longer be pending"
        );
        assert_eq!(
            SlackInboxProducerCheckpoint::from_checkpoint(&second_poll.checkpoint)
                .delivered_ingress_ids,
            Vec::<IngressId>::new()
        );
    }

    #[tokio::test]
    async fn inbox_producer_does_not_tight_loop_reemit_without_timeout() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let (_store_guard, store) = install_memory_ingress_store();
        let source_key =
            crate::channel::slack_polling_source_key("C123ABC456").expect("valid source key");
        let batch_messages = [json!({
            "type": "message",
            "user": "U123",
            "text": "Track the OAuth docs follow-up.",
            "ts": "1700000001.000001",
            "thread_ts": "1700000001.000001"
        })];
        let normalized_batch = normalize_polling_batch(
            RAW_SOURCE_SCHEMA_VERSION,
            "C123ABC456",
            &source_key,
            "C123ABC456",
            &batch_messages,
            1_700_000_100,
        );
        let ingress_item = IngressItem {
            ingress_id: IngressId::parse("support/slack:test-inbox-001").expect("valid ingress id"),
            source_kind: slack_source_kind().expect("valid slack source kind"),
            source_key: source_key.clone(),
            payload_json: serde_json::to_string(&normalized_batch).expect("serialize batch"),
            enqueued_at_unix_ms: 1_700_000_100_000,
        };
        store
            .enqueue(&ingress_item)
            .await
            .expect("enqueue sample durable ingress item");

        let producer = SlackInboxEventProducer::new(store.clone()).expect("build inbox producer");
        let first_poll = producer
            .poll(&ProducerCheckpoint::none())
            .await
            .expect("first inbox poll should emit the durable item");
        assert_eq!(first_poll.events.len(), 1);

        let immediate_retry = producer
            .poll(&ProducerCheckpoint::none())
            .await
            .expect("second inbox poll should not immediately re-emit");
        assert!(
            immediate_retry.events.is_empty(),
            "emitted inbox items should wait for retry timeout before re-emission"
        );

        store.set_emitted_at(&ingress_item.ingress_id, 0).await;

        let reclaimed_retry = producer
            .poll(&ProducerCheckpoint::none())
            .await
            .expect("stale emitted ingress should be retried");
        assert_eq!(reclaimed_retry.events.len(), 1);
    }

    #[tokio::test]
    async fn polling_transport_registers_no_rest_producers_without_ingress_store() {
        use baml_rt_tools::{InventoryCatalog, ToolCatalog, ToolName};

        let _guard = crate::slack_test_env_lock().lock().await;
        let metadata = InventoryCatalog::new()
            .by_name(&ToolName::parse("support/slack").expect("valid tool name"))
            .cloned()
            .expect("support/slack metadata");

        let producers = build_slack_event_producers(EventProducerBuildContext {
            metadata,
            config: Some(json!({
                "channels": ["ops", "C123ABC456"]
            })),
            persisted_checkpoints: Arc::new(HashMap::new()),
            ingress_store: None,
        })
        .await
        .expect("producer build should succeed");

        assert!(
            producers.is_empty(),
            "REST channel polling is owned by task-daemon; runner should not register SlackEventProducer"
        );
    }

    #[tokio::test]
    async fn resolve_selector_channel_id_paginates_conversations_list() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let state = MockState::default();
        let app = Router::new().route(
            "/api/conversations.list",
            get({
                let state = state.clone();
                move |Query(query): Query<HashMap<String, String>>| {
                    let state = state.clone();
                    async move {
                        let cursor = query.get("cursor").cloned();
                        state.push_hit(format!("list cursor={cursor:?}")).await;
                        let body = match cursor.as_deref() {
                            None => json!({
                                "ok": true,
                                "channels": [{ "id": "C11111111", "name": "general" }],
                                "response_metadata": { "next_cursor": "cursor-2" }
                            }),
                            Some("cursor-2") => json!({
                                "ok": true,
                                "channels": [{ "id": "C22222222", "name": "random" }],
                                "response_metadata": { "next_cursor": "cursor-3" }
                            }),
                            Some("cursor-3") => json!({
                                "ok": true,
                                "channels": [{ "id": "C33333333", "name": "alerts" }],
                                "response_metadata": { "next_cursor": "cursor-4" }
                            }),
                            Some("cursor-4") => json!({
                                "ok": true,
                                "channels": [{ "id": "C123ABC456", "name": "ops" }],
                                "response_metadata": { "next_cursor": "" }
                            }),
                            other => panic!("unexpected cursor: {other:?}"),
                        };
                        Json(body)
                    }
                }
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);

        let client = SlackReadClient::new();
        let channel_id = crate::channel::resolve_selector_channel_id(
            &client,
            "xoxb-test",
            &SlackChannelSelector::ChannelName("ops".into()),
        )
        .await
        .expect("resolve channel name across pages");

        assert_eq!(channel_id, "C123ABC456");
        assert_eq!(
            state.snapshot().await,
            vec![
                "list cursor=None".to_string(),
                "list cursor=Some(\"cursor-2\")".to_string(),
                "list cursor=Some(\"cursor-3\")".to_string(),
                "list cursor=Some(\"cursor-4\")".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn resolve_selector_channel_id_uses_channel_id_without_list() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let state = MockState::default();
        let app = Router::new().route(
            "/api/conversations.list",
            get({
                let state = state.clone();
                move || {
                    let state = state.clone();
                    async move {
                        state.push_hit("list".to_string()).await;
                        Json(json!({
                            "ok": false,
                            "error": "should-not-call-list"
                        }))
                    }
                }
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);

        let client = SlackReadClient::new();
        let channel_id = crate::channel::resolve_selector_channel_id(
            &client,
            "xoxb-test",
            &SlackChannelSelector::ChannelId("C123ABC456".into()),
        )
        .await
        .expect("channel id selector should not call conversations.list");

        assert_eq!(channel_id, "C123ABC456");
        assert!(
            state.snapshot().await.is_empty(),
            "channel id resolution should not call conversations.list"
        );
    }

    #[test]
    fn config_dedupes_duplicate_channel_entries() {
        let config = SlackEventProducerConfig {
            channels: vec!["ops".into(), "ops".into()],
            ..Default::default()
        };
        let channels = config.normalized_channels().expect("normalize channels");
        assert_eq!(channels.len(), 1);
        assert!(matches!(
            channels[0],
            SlackChannelSelector::ChannelName(ref name) if name == "ops"
        ));
    }

    #[tokio::test]
    async fn build_registers_inbox_producer_without_polling_channels() {
        use baml_rt_tools::{InventoryCatalog, ToolCatalog, ToolName};

        let _guard = crate::slack_test_env_lock().lock().await;
        let (_store_guard, store) = install_memory_ingress_store();
        let metadata = InventoryCatalog::new()
            .by_name(&ToolName::parse("support/slack").expect("valid tool name"))
            .cloned()
            .expect("support/slack metadata");

        let producers = build_slack_event_producers(EventProducerBuildContext {
            metadata,
            config: Some(json!({
                "channels": []
            })),
            persisted_checkpoints: Arc::new(HashMap::new()),
            ingress_store: Some(store),
        })
        .await
        .expect("producer build should still register the durable inbox producer");

        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].producer_key(), "support/slack:inbox");
    }
}
