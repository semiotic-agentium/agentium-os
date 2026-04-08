//! Host-managed Slack event producer registration.
//!
//! This operationalizes `support/slack` as an event source without hard-coding
//! Slack-specific registration in the runner.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{
    AgentDispatchRoutingKey, BamlRtError, EventSchemaVersion, EventSourceKind, ProducedEvent,
    Result,
    event_subscription::EventSourceKey,
    ingress_store::{IngressId, IngressItem},
};
use baml_rt_tools::{
    EventProducer, EventProducerBuildContext, EventProducerBuildFuture, EventProducerProvider,
    ProducerCheckpoint, ProducerPoll, ingress_store::ingress_store,
};
use integrations_slack_read::{SlackAuthPreference, SlackReadClient, SlackReadError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::{
    SlackError,
    normalize::{SlackNormalizedBatch, normalize_polling_batch},
};

/// Generic raw ingress schema for host-managed source records.
pub const RAW_SOURCE_SCHEMA_VERSION: &str = "host.source-records.v1";
/// Generic intake routing key for raw source ingress.
pub const RAW_SOURCE_ROUTING_KEY: &str = "event:intake";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SlackChannelSelector {
    ChannelId(String),
    ChannelName(String),
}

impl SlackChannelSelector {
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim().trim_start_matches('#');
        if trimmed.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "support/slack config contains an empty channel entry".to_string(),
            ));
        }

        if looks_like_channel_id(trimmed) {
            Ok(Self::ChannelId(trimmed.to_ascii_uppercase()))
        } else {
            Ok(Self::ChannelName(trimmed.to_ascii_lowercase()))
        }
    }

    pub(crate) fn producer_key_fragment(&self) -> String {
        match self {
            Self::ChannelId(id) => format!("id:{id}"),
            Self::ChannelName(name) => format!("name:{name}"),
        }
    }

    pub(crate) fn display_label(&self) -> String {
        match self {
            Self::ChannelId(id) => id.clone(),
            Self::ChannelName(name) => format!("#{name}"),
        }
    }

    pub(crate) fn needs_resolution(&self) -> bool {
        matches!(self, Self::ChannelName(_))
    }
}

/// Transport mode for Slack event ingestion.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlackTransportConfig {
    /// Poll conversations.history on a timer (existing behavior).
    #[default]
    Polling,
    /// Connect via Slack Socket Mode WebSocket. Requires SLACK_APP_TOKEN.
    SocketMode,
}

/// Config for host-managed Slack source ingestion.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType, Default)]
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
struct SlackCheckpoint {
    /// Slack timestamp of the latest committed message (exclusive lower bound for future polls).
    last_seen_ts: Option<String>,
    /// Latest timestamp observed while a page-limited backfill is still in progress.
    pending_last_seen_ts: Option<String>,
    /// Exclusive upper bound for continuing a backfill window on the next poll.
    backfill_latest_ts: Option<String>,
    /// Cached channel ID (avoids re-resolving name → ID each poll).
    resolved_channel_id: Option<String>,
}

impl SlackCheckpoint {
    fn from_producer_checkpoint(checkpoint: &ProducerCheckpoint) -> Self {
        match checkpoint.value() {
            Some(s) => match serde_json::from_str(s) {
                Ok(cp) => cp,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "corrupt Slack checkpoint; resetting to default (24h lookback)"
                    );
                    Self::default()
                }
            },
            None => Self::default(),
        }
    }

    fn to_producer_checkpoint(&self) -> ProducerCheckpoint {
        match serde_json::to_string(self) {
            Ok(s) => ProducerCheckpoint::some(s),
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "failed to serialize Slack checkpoint; cursor will not advance"
                );
                ProducerCheckpoint::none()
            }
        }
    }
}

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

/// Default lookback window on first poll (24 hours).
const INITIAL_LOOKBACK_SECS: u64 = 86_400;
/// Maximum messages per API call.
const HISTORY_LIMIT: u16 = 200;
/// Maximum pages to fetch per poll cycle (bounds API calls per interval).
const MAX_PAGES: usize = 3;
/// Maximum persisted Slack ingress items emitted by the inbox producer per poll.
const MAX_SLACK_INBOX_ITEMS_PER_POLL: usize = 100;
/// Stable producer identity for the durable Slack inbox.
const SLACK_INBOX_PRODUCER_KEY: &str = "support/slack:inbox";
/// Wait one minute before retrying an emitted-but-unconfirmed durable inbox item.
const SLACK_INGRESS_RETRY_AFTER_MS: u64 = 60_000;

struct HistoryFetch {
    messages: Vec<serde_json::Value>,
    has_more: bool,
}

struct ResolvedSlackProducerSeed {
    channel: SlackChannelSelector,
    producer_key: String,
    producer_key_priority: ProducerKeyPriority,
    resolved_channel_id: String,
}

#[derive(Debug, Clone)]
struct PersistedSlackProducerState {
    producer_key: String,
    state: SlackCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProducerKeyPriority {
    Fresh = 0,
    ResolvedIdMatch = 1,
    ExactPersistedMatch = 2,
}

#[derive(Debug, Clone)]
struct SlackProducerCandidate {
    channel: SlackChannelSelector,
    producer_key: String,
    producer_key_priority: ProducerKeyPriority,
    resolved_channel_id: String,
}

/// Polls a single Slack channel for new messages and emits them as generic
/// raw source-record batches.
pub struct SlackEventProducer {
    client: SlackReadClient,
    channel: SlackChannelSelector,
    producer_key: String,
    source_kinds: Vec<EventSourceKind>,
    /// Cached channel ID resolved from name. This stays mutable so stale ids
    /// can be discarded and re-resolved without a restart.
    resolved_channel_id: tokio::sync::RwLock<Option<String>>,
}

pub struct SlackInboxEventProducer {
    producer_key: &'static str,
    routing_key: AgentDispatchRoutingKey,
    schema_version: EventSchemaVersion,
    source_kind: EventSourceKind,
    source_kinds: Vec<EventSourceKind>,
}

impl SlackEventProducer {
    /// Create a new producer for the given channel (name or ID).
    ///
    /// Accepts a shared `SlackReadClient` so the build function and all
    /// producers reuse a single `reqwest::Client` and connection pool.
    /// Token availability is validated on every [`poll`] cycle, so there
    /// is no need for an eager check here.
    fn new(
        client: SlackReadClient,
        channel: SlackChannelSelector,
        producer_key: String,
        initial_resolved_channel_id: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            client,
            producer_key,
            channel,
            source_kinds: vec![slack_source_kind()?],
            resolved_channel_id: tokio::sync::RwLock::new(initial_resolved_channel_id),
        })
    }

    async fn cached_channel_id(&self) -> Option<String> {
        self.resolved_channel_id.read().await.clone()
    }

    async fn set_cached_channel_id(&self, channel_id: String) {
        *self.resolved_channel_id.write().await = Some(channel_id);
    }

    async fn clear_cached_channel_id(&self) {
        *self.resolved_channel_id.write().await = None;
    }

    /// Resolve the channel name to an ID. Checks the in-memory cache first,
    /// then the checkpoint, then calls conversations.list as a last resort.
    async fn resolve_channel_id(
        &self,
        token: &str,
        checkpoint: &SlackCheckpoint,
        use_cached_resolution: bool,
    ) -> Result<String> {
        match &self.channel {
            SlackChannelSelector::ChannelId(channel_id) => {
                self.set_cached_channel_id(channel_id.clone()).await;
                Ok(channel_id.clone())
            }
            SlackChannelSelector::ChannelName(target) => {
                if use_cached_resolution {
                    if let Some(id) = self.cached_channel_id().await {
                        return Ok(id);
                    }
                    if let Some(ref id) = checkpoint.resolved_channel_id {
                        self.set_cached_channel_id(id.clone()).await;
                        return Ok(id.clone());
                    }
                }

                let id = resolve_channel_name(&self.client, token, target).await?;
                self.set_cached_channel_id(id.clone()).await;
                Ok(id)
            }
        }
    }

    /// Fetch messages newer than `oldest` and older than `latest` from the
    /// channel, paginating up to `MAX_PAGES` pages to avoid unbounded API calls.
    async fn fetch_messages(
        &self,
        token: &str,
        channel_id: &str,
        oldest: Option<&str>,
        latest: Option<&str>,
    ) -> std::result::Result<HistoryFetch, SlackReadError> {
        let mut all_messages = Vec::new();
        let mut cursor: Option<String> = None;
        let mut has_more = false;

        let oldest_param = match oldest {
            Some(ts) => ts.to_string(),
            None => {
                let lookback = baml_rt_core::now_unix_secs("slack_lookback")
                    .saturating_sub(INITIAL_LOOKBACK_SECS);
                format!("{lookback}.000000")
            }
        };

        for _ in 0..MAX_PAGES {
            let mut query = vec![
                ("channel", channel_id.to_string()),
                ("limit", HISTORY_LIMIT.to_string()),
                ("oldest", oldest_param.clone()),
            ];
            if let Some(ts) = latest {
                query.push(("latest", ts.to_string()));
            }
            query.push(("inclusive", "false".to_string()));
            if let Some(ref c) = cursor {
                query.push(("cursor", c.clone()));
            }
            let json = self
                .client
                .get_json("conversations.history", token, &query)
                .await?;

            if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
                all_messages.extend(messages.iter().cloned());
            }

            let next_cursor = json
                .pointer("/response_metadata/next_cursor")
                .and_then(|c| c.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            has_more = json
                .get("has_more")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                && next_cursor.is_some();
            if !has_more {
                break;
            }
            match next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        all_messages.reverse();
        Ok(HistoryFetch {
            messages: all_messages,
            has_more,
        })
    }
}

impl SlackInboxEventProducer {
    fn new() -> Result<Self> {
        let (routing_key, schema_version, source_kind) = raw_source_dispatch_contract()?;
        Ok(Self {
            producer_key: SLACK_INBOX_PRODUCER_KEY,
            routing_key,
            schema_version,
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
        Ok(ProducedEvent {
            routing_key: self.routing_key.clone(),
            schema_version: self.schema_version.clone(),
            source_kind: self.source_kind.clone(),
            source_key: item.source_key.clone(),
            messages: vec![payload],
            context_id: None,
            task_id: None,
            message_id: Some(item.ingress_id.to_string()),
            metadata: None,
        })
    }
}

fn raw_source_dispatch_contract()
-> Result<(AgentDispatchRoutingKey, EventSchemaVersion, EventSourceKind)> {
    let routing_key = AgentDispatchRoutingKey::parse(RAW_SOURCE_ROUTING_KEY).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "invalid static Slack routing key '{routing_key}'",
            routing_key = RAW_SOURCE_ROUTING_KEY
        ))
    })?;
    let schema_version = EventSchemaVersion::parse(RAW_SOURCE_SCHEMA_VERSION).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "invalid static Slack schema version '{schema_version}'",
            schema_version = RAW_SOURCE_SCHEMA_VERSION
        ))
    })?;
    let source_kind = slack_source_kind()?;
    Ok((routing_key, schema_version, source_kind))
}

fn slack_source_kind() -> Result<EventSourceKind> {
    EventSourceKind::parse("slack").ok_or_else(|| {
        BamlRtError::InvalidArgument("invalid static Slack source kind 'slack'".to_string())
    })
}

#[async_trait]
impl EventProducer for SlackEventProducer {
    fn producer_key(&self) -> &str {
        &self.producer_key
    }

    fn source_kinds(&self) -> &[EventSourceKind] {
        &self.source_kinds
    }

    async fn poll(&self, checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll> {
        let mut state = SlackCheckpoint::from_producer_checkpoint(checkpoint);

        let (token_ref, _kind) = self
            .client
            .select_token(Some(SlackAuthPreference::Auto), false)
            .map_err(|err| {
                BamlRtError::ToolExecution(format!("Slack token selection failed: {err}"))
            })?;
        let token = token_ref.to_string();

        let mut channel_id = self.resolve_channel_id(&token, &state, true).await?;
        state.resolved_channel_id = Some(channel_id.clone());

        let fetch = match self
            .fetch_messages(
                &token,
                &channel_id,
                state.last_seen_ts.as_deref(),
                state.backfill_latest_ts.as_deref(),
            )
            .await
        {
            Ok(fetch) => fetch,
            Err(error)
                if self.channel.needs_resolution() && is_stale_channel_resolution_error(&error) =>
            {
                warn!(
                    channel = %self.channel.display_label(),
                    stale_channel_id = %channel_id,
                    "cached Slack channel id is stale; re-resolving by channel name"
                );
                self.clear_cached_channel_id().await;
                state.resolved_channel_id = None;
                channel_id = self.resolve_channel_id(&token, &state, false).await?;
                state.resolved_channel_id = Some(channel_id.clone());
                self.fetch_messages(
                    &token,
                    &channel_id,
                    state.last_seen_ts.as_deref(),
                    state.backfill_latest_ts.as_deref(),
                )
                .await
                .map_err(map_slack_read_error)?
            }
            Err(error) => return Err(map_slack_read_error(error)),
        };

        let raw_latest_ts = latest_message_ts(&fetch.messages);
        let raw_earliest_ts = earliest_message_ts(&fetch.messages);
        let mut messages = fetch.messages;
        if let Some(last_seen_ts) = state.last_seen_ts.as_deref() {
            messages.retain(|message| {
                message
                    .get("ts")
                    .and_then(|ts| ts.as_str())
                    .is_some_and(|ts| ts_gt(ts, last_seen_ts))
            });
        }
        let batch_latest_ts = latest_message_ts(&messages);
        let continue_backfill = fetch.has_more;

        if continue_backfill {
            if let Some(raw_earliest_ts) = raw_earliest_ts {
                state.backfill_latest_ts = Some(raw_earliest_ts);
            }
            state.pending_last_seen_ts = max_ts(
                state.pending_last_seen_ts.as_deref(),
                raw_latest_ts.as_deref(),
            );
            warn!(
                channel = %self.channel.display_label(),
                channel_id = %channel_id,
                message_count = messages.len(),
                "Slack history exceeded max_pages; continuing backfill on next poll"
            );
        } else {
            let committed_ts = max_ts(
                state.pending_last_seen_ts.as_deref(),
                batch_latest_ts.as_deref(),
            );
            state.pending_last_seen_ts = None;
            state.backfill_latest_ts = None;
            if let Some(committed_ts) = committed_ts {
                state.last_seen_ts = Some(committed_ts);
            }
        }

        if messages.is_empty() {
            debug!(
                channel = %self.channel.display_label(),
                channel_id = %channel_id,
                "no new Slack messages"
            );
            return Ok(ProducerPoll {
                events: vec![],
                checkpoint: state.to_producer_checkpoint(),
            });
        }

        let source_key = polling_source_key(&channel_id)?;
        let ingress_id = polling_ingress_id(&source_key, &messages)?;
        info!(
            channel = %self.channel.display_label(),
            channel_id = %channel_id,
            message_count = messages.len(),
            "polled new Slack messages"
        );

        let normalized_batch = normalize_polling_batch(
            RAW_SOURCE_SCHEMA_VERSION,
            &channel_id,
            &source_key,
            &self.channel.display_label(),
            &messages,
            emitted_at_unix(),
        );
        if let Some(store) = ingress_store() {
            let enqueued_at_unix_ms = emitted_at_unix_ms();
            let payload_json = serde_json::to_string(&normalized_batch).map_err(|err| {
                BamlRtError::InvalidArgument(format!(
                    "failed to serialize Slack ingress payload: {err}"
                ))
            })?;
            let enqueued = store
                .enqueue(&IngressItem {
                    ingress_id: ingress_id.clone(),
                    source_key: source_key.clone(),
                    payload_json,
                    enqueued_at_unix_ms,
                })
                .await?;
            if !enqueued {
                warn!(
                    producer_key = %self.producer_key(),
                    ingress_id = %ingress_id,
                    "Slack poll produced a duplicate durable ingress item; checkpoint will still advance"
                );
            }
            return Ok(ProducerPoll {
                events: vec![],
                checkpoint: state.to_producer_checkpoint(),
            });
        }

        let event = normalized_batch_to_event(normalized_batch, ingress_id)?;

        Ok(ProducerPoll {
            events: vec![event],
            checkpoint: state.to_producer_checkpoint(),
        })
    }
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
        let store = ingress_store().ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "support/slack inbox producer requires an installed ingress store".to_string(),
            )
        })?;
        let now_unix_ms = emitted_at_unix_ms();
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

        let pending_items = store.list_pending(MAX_SLACK_INBOX_ITEMS_PER_POLL).await?;
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
        let persisted_states = persisted_slack_producer_states(&ctx);
        let store_installed = ingress_store().is_some();
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
                let store = ingress_store().ok_or_else(|| {
                    BamlRtError::InvalidArgument(
                        "Socket Mode transport requires an installed ingress store".to_string(),
                    )
                })?;
                let client = SlackReadClient::new();
                let app_token = client.auth().app_token.clone().ok_or_else(|| {
                    BamlRtError::InvalidArgument(
                        "Socket Mode transport requires SLACK_APP_TOKEN (xapp-...)".to_string(),
                    )
                })?;

                let _handle = crate::socket_mode::start_socket_mode_receiver(
                    client, app_token, channels, store,
                )
                .await?;

                let producers: Vec<Arc<dyn EventProducer>> =
                    vec![Arc::new(SlackInboxEventProducer::new()?) as Arc<dyn EventProducer>];
                Ok(producers)
            }
            SlackTransportConfig::Polling => {
                if channels.is_empty() && !store_installed {
                    return Ok(vec![]);
                }

                let client = SlackReadClient::new();
                let mut selected_token: Option<String> = None;
                let mut candidates = Vec::<SlackProducerCandidate>::new();
                for channel in channels {
                    let mut producer_key = selector_producer_key(&channel);
                    let mut producer_key_priority = ProducerKeyPriority::Fresh;
                    let mut resolved_channel_id =
                        persisted_state_for_selector(&persisted_states, &channel).and_then(
                            |persisted| {
                                producer_key = persisted.producer_key.clone();
                                producer_key_priority = ProducerKeyPriority::ExactPersistedMatch;
                                persisted.state.resolved_channel_id.clone().or_else(|| {
                                    match &channel {
                                        SlackChannelSelector::ChannelId(channel_id) => {
                                            Some(channel_id.clone())
                                        }
                                        SlackChannelSelector::ChannelName(_) => None,
                                    }
                                })
                            },
                        );

                    if resolved_channel_id.is_none() {
                        let token = match selected_token.as_ref() {
                            Some(token) => token.clone(),
                            None => {
                                let (token_ref, _kind) = client
                                    .select_token(Some(SlackAuthPreference::Auto), false)
                                    .map_err(|err| {
                                        BamlRtError::InvalidArgument(format!(
                                            "Slack event producer requires SLACK_BOT_TOKEN or SLACK_USER_TOKEN: {err}"
                                        ))
                                    })?;
                                let token_value = token_ref.to_string();
                                selected_token = Some(token_value.clone());
                                token_value
                            }
                        };
                        let resolved =
                            resolve_selector_channel_id(&client, &token, &channel).await?;
                        if producer_key_priority < ProducerKeyPriority::ExactPersistedMatch
                            && let Some(persisted) = persisted_state_for_resolved_channel_id(
                                &persisted_states,
                                &resolved,
                            )
                        {
                            producer_key = persisted.producer_key.clone();
                            producer_key_priority = ProducerKeyPriority::ResolvedIdMatch;
                        }
                        resolved_channel_id = Some(resolved);
                    }

                    let resolved_channel_id = resolved_channel_id.ok_or_else(|| {
                        BamlRtError::InvalidArgument(format!(
                            "Slack producer for {} could not determine a channel ID",
                            channel.display_label()
                        ))
                    })?;
                    candidates.push(SlackProducerCandidate {
                        channel,
                        producer_key,
                        producer_key_priority,
                        resolved_channel_id,
                    });
                }

                let mut deduped = Vec::<ResolvedSlackProducerSeed>::new();
                let mut by_channel_id = HashMap::<String, usize>::new();
                for candidate in candidates {
                    match by_channel_id.get(&candidate.resolved_channel_id).copied() {
                        Some(existing_idx) => {
                            let existing = &mut deduped[existing_idx];
                            if candidate.channel.needs_resolution()
                                && !existing.channel.needs_resolution()
                            {
                                existing.channel = candidate.channel.clone();
                            }
                            if candidate.producer_key_priority > existing.producer_key_priority {
                                existing.producer_key = candidate.producer_key.clone();
                                existing.producer_key_priority = candidate.producer_key_priority;
                            }
                        }
                        None => {
                            by_channel_id
                                .insert(candidate.resolved_channel_id.clone(), deduped.len());
                            deduped.push(ResolvedSlackProducerSeed {
                                channel: candidate.channel,
                                producer_key: candidate.producer_key,
                                producer_key_priority: candidate.producer_key_priority,
                                resolved_channel_id: candidate.resolved_channel_id,
                            });
                        }
                    }
                }

                let mut producers: Vec<Arc<dyn EventProducer>> = Vec::new();
                for seed in deduped {
                    producers.push(Arc::new(SlackEventProducer::new(
                        client.clone(),
                        seed.channel,
                        seed.producer_key,
                        Some(seed.resolved_channel_id),
                    )?) as Arc<dyn EventProducer>);
                }
                if store_installed {
                    producers
                        .push(Arc::new(SlackInboxEventProducer::new()?) as Arc<dyn EventProducer>);
                }
                Ok(producers)
            }
        }
    })
}

fn persisted_slack_producer_states(
    ctx: &EventProducerBuildContext,
) -> Vec<PersistedSlackProducerState> {
    ctx.persisted_checkpoints
        .iter()
        .filter(|(producer_key, _)| {
            producer_key.starts_with("support/slack:")
                && producer_key.as_str() != SLACK_INBOX_PRODUCER_KEY
        })
        .map(|(producer_key, checkpoint)| PersistedSlackProducerState {
            producer_key: producer_key.clone(),
            state: SlackCheckpoint::from_producer_checkpoint(checkpoint),
        })
        .collect()
}

fn selector_producer_key(selector: &SlackChannelSelector) -> String {
    format!("support/slack:{}", selector.producer_key_fragment())
}

fn persisted_state_for_selector<'a>(
    persisted_states: &'a [PersistedSlackProducerState],
    selector: &SlackChannelSelector,
) -> Option<&'a PersistedSlackProducerState> {
    let producer_key = selector_producer_key(selector);
    persisted_states
        .iter()
        .find(|persisted| persisted.producer_key == producer_key)
}

fn persisted_state_for_resolved_channel_id<'a>(
    persisted_states: &'a [PersistedSlackProducerState],
    resolved_channel_id: &str,
) -> Option<&'a PersistedSlackProducerState> {
    persisted_states.iter().find(|persisted| {
        persisted.state.resolved_channel_id.as_deref() == Some(resolved_channel_id)
    })
}

pub(crate) fn looks_like_channel_id(raw: &str) -> bool {
    raw.len() >= 9
        && raw.chars().skip(1).any(|ch| ch.is_ascii_digit())
        && raw.chars().enumerate().all(|(idx, ch)| match idx {
            0 => matches!(ch, 'C' | 'D' | 'G'),
            _ => ch.is_ascii_uppercase() || ch.is_ascii_digit(),
        })
}

fn map_slack_read_error(error: SlackReadError) -> BamlRtError {
    BamlRtError::from(SlackError::from(error))
}

pub(crate) async fn resolve_selector_channel_id(
    client: &SlackReadClient,
    token: &str,
    selector: &SlackChannelSelector,
) -> Result<String> {
    match selector {
        SlackChannelSelector::ChannelId(channel_id) => Ok(channel_id.clone()),
        SlackChannelSelector::ChannelName(target) => {
            resolve_channel_name(client, token, target).await
        }
    }
}

async fn resolve_channel_name(
    client: &SlackReadClient,
    token: &str,
    target: &str,
) -> Result<String> {
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    loop {
        let mut query = vec![
            ("types", "public_channel,private_channel".to_string()),
            ("exclude_archived", "true".to_string()),
            ("limit", "200".to_string()),
        ];
        if let Some(ref c) = cursor {
            query.push(("cursor", c.clone()));
        }
        let json = client
            .get_json("conversations.list", token, &query)
            .await
            .map_err(map_slack_read_error)?;
        if let Some(channels) = json.get("channels").and_then(|c| c.as_array()) {
            for ch in channels {
                let name = ch.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name.eq_ignore_ascii_case(target)
                    && let Some(id) = ch.get("id").and_then(|i| i.as_str())
                {
                    return Ok(id.to_string());
                }
            }
        }
        let next_cursor = json
            .pointer("/response_metadata/next_cursor")
            .and_then(|c| c.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        match next_cursor {
            Some(c) => {
                if !seen_cursors.insert(c.clone()) {
                    return Err(BamlRtError::ToolExecution(format!(
                        "Slack channel resolution for '{target}' encountered a repeated cursor"
                    )));
                }
                cursor = Some(c);
            }
            None => break,
        }
    }
    Err(BamlRtError::InvalidArgument(format!(
        "Slack channel '{target}' not found"
    )))
}

fn is_stale_channel_resolution_error(error: &SlackReadError) -> bool {
    matches!(
        error,
        SlackReadError::ApiError { method, error, .. }
            if *method == "conversations.history"
                && matches!(error.as_str(), "channel_not_found" | "not_in_channel")
    )
}

fn earliest_message_ts(messages: &[serde_json::Value]) -> Option<String> {
    messages
        .iter()
        .find_map(|message| message.get("ts").and_then(|ts| ts.as_str()))
        .map(ToString::to_string)
}

fn latest_message_ts(messages: &[serde_json::Value]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find_map(|message| message.get("ts").and_then(|ts| ts.as_str()))
        .map(ToString::to_string)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SlackTimestamp {
    seconds: u64,
    micros: u32,
}

fn parse_slack_timestamp(ts: &str) -> Option<SlackTimestamp> {
    let trimmed = ts.trim();
    let (seconds, fractional) = match trimmed.split_once('.') {
        Some((seconds, fractional)) => (seconds, Some(fractional)),
        None => (trimmed, None),
    };

    if seconds.is_empty() || !seconds.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let seconds = seconds.parse::<u64>().ok()?;

    let micros = match fractional {
        Some(fractional) => {
            if !fractional.chars().all(|ch| ch.is_ascii_digit()) {
                return None;
            }
            let mut micros = fractional.chars().take(6).collect::<String>();
            while micros.len() < 6 {
                micros.push('0');
            }
            micros.parse::<u32>().ok()?
        }
        None => 0,
    };

    Some(SlackTimestamp { seconds, micros })
}

fn ts_gt(left: &str, right: &str) -> bool {
    match (parse_slack_timestamp(left), parse_slack_timestamp(right)) {
        (Some(left), Some(right)) => left > right,
        _ => left > right,
    }
}

fn ts_cmp(left: &str, right: &str) -> Ordering {
    match (parse_slack_timestamp(left), parse_slack_timestamp(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn max_ts(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => {
            if ts_cmp(left, right).is_gt() {
                Some(left.to_string())
            } else {
                Some(right.to_string())
            }
        }
        (Some(left), None) => Some(left.to_string()),
        (None, Some(right)) => Some(right.to_string()),
        (None, None) => None,
    }
}

fn normalized_batch_to_event(
    batch: SlackNormalizedBatch,
    message_id: IngressId,
) -> Result<ProducedEvent> {
    let (routing_key, schema_version, source_kind) = raw_source_dispatch_contract()?;
    let source_key = batch.source.source_key.clone();
    let payload = serde_json::to_value(&batch).map_err(|err| {
        BamlRtError::InvalidArgument(format!(
            "failed to serialize normalized Slack batch for dispatch: {err}"
        ))
    })?;
    Ok(ProducedEvent {
        routing_key,
        schema_version,
        source_kind,
        source_key,
        messages: vec![payload],
        context_id: None,
        task_id: None,
        message_id: Some(message_id.to_string()),
        metadata: None,
    })
}

fn polling_source_key(channel_id: &str) -> Result<EventSourceKey> {
    EventSourceKey::parse(format!("slack:{channel_id}")).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "invalid Slack source key derived from channel ID: slack:{channel_id}"
        ))
    })
}

fn polling_ingress_id(
    source_key: &EventSourceKey,
    messages: &[serde_json::Value],
) -> Result<IngressId> {
    let earliest_ts = earliest_message_ts(messages);
    let latest_ts = latest_message_ts(messages);
    let batch_fingerprint = polling_batch_fingerprint(messages)?;
    IngressId::parse(format!(
        "support/slack:poll:{source_key}:{earliest}:{latest}:{message_count}:{batch_fingerprint}",
        earliest = earliest_ts.as_deref().unwrap_or("none"),
        latest = latest_ts.as_deref().unwrap_or("none"),
        message_count = messages.len(),
    ))
    .ok_or_else(|| {
        BamlRtError::InvalidArgument("generated polling ingress ID must not be empty".to_string())
    })
}

fn polling_batch_fingerprint(messages: &[serde_json::Value]) -> Result<String> {
    let mut identities = messages
        .iter()
        .enumerate()
        .map(|(index, message)| canonical_polling_message_identity(index, message))
        .collect::<Result<Vec<_>>>()?;
    identities.sort();

    let mut hasher = Sha256::new();
    for identity in identities {
        hasher.update(identity.as_bytes());
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn canonical_polling_message_identity(index: usize, message: &serde_json::Value) -> Result<String> {
    if let Some(ts) = message.get("ts").and_then(serde_json::Value::as_str) {
        return Ok(format!("ts:{ts}"));
    }

    serde_json::to_string(message)
        .map(|json| format!("json:{index}:{json}"))
        .map_err(|err| {
            BamlRtError::InvalidArgument(format!(
                "failed to serialize Slack polling message for ingress fingerprint: {err}"
            ))
        })
}

fn emitted_at_unix() -> u64 {
    baml_rt_core::now_unix_secs("slack_poll_normalize")
}

fn emitted_at_unix_ms() -> u64 {
    baml_rt_core::now_unix_ms("slack_ingress")
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

    use axum::{Json, Router, extract::Query, http::HeaderMap, routing::get};
    use baml_rt_core::IngressStore;
    use baml_rt_tools::ProducerCheckpoint;
    use serde_json::json;
    use test_support::common::TempEnvVar;

    use super::*;
    use crate::test_support::install_memory_ingress_store;

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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(format!("http://{addr}/api"))
    }

    fn persisted_checkpoint_for_channel_id(channel_id: &str) -> ProducerCheckpoint {
        SlackCheckpoint {
            last_seen_ts: Some("1700000001.000000".into()),
            pending_last_seen_ts: None,
            backfill_latest_ts: None,
            resolved_channel_id: Some(channel_id.into()),
        }
        .to_producer_checkpoint()
    }

    #[test]
    fn config_rejects_blank_channels() {
        let config = SlackEventProducerConfig {
            channels: vec!["   ".into()],
            ..Default::default()
        };
        let err = config.normalized_channels().unwrap_err().to_string();
        assert!(err.contains("empty channel entry"));
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

    #[test]
    fn polling_ingress_id_changes_when_batch_members_change() {
        let source_key = polling_source_key("C123ABC456").expect("valid source key");
        let first = polling_ingress_id(
            &source_key,
            &[
                json!({ "ts": "1700000001.000001" }),
                json!({ "ts": "1700000002.000001" }),
                json!({ "ts": "1700000004.000001" }),
            ],
        )
        .expect("first ingress id");
        let second = polling_ingress_id(
            &source_key,
            &[
                json!({ "ts": "1700000001.000001" }),
                json!({ "ts": "1700000003.000001" }),
                json!({ "ts": "1700000004.000001" }),
            ],
        )
        .expect("second ingress id");

        assert_ne!(first, second);
    }

    #[test]
    fn config_dedupes_canonicalized_channels() {
        let config = SlackEventProducerConfig {
            channels: vec!["ops".into(), "#ops".into(), " Ops ".into()],
            ..Default::default()
        };
        assert_eq!(
            config.normalized_channels().unwrap(),
            vec![SlackChannelSelector::ChannelName("ops".to_string())]
        );
    }

    #[test]
    fn producer_key_is_tool_namespaced() {
        let producer = SlackEventProducer::new(
            SlackReadClient::new(),
            SlackChannelSelector::ChannelName("agentium-eng".into()),
            "support/slack:name:agentium-eng".into(),
            Some("C123ABC456".into()),
        )
        .expect("build test producer");
        assert_eq!(producer.producer_key(), "support/slack:name:agentium-eng");
        assert_eq!(producer.source_kinds()[0].as_str(), "slack");
    }

    #[test]
    fn source_label_prefixes_channel_names_only() {
        assert_eq!(
            SlackChannelSelector::ChannelName("agentium-eng".into()).display_label(),
            "#agentium-eng"
        );
        assert_eq!(
            SlackChannelSelector::ChannelName("ops".into()).display_label(),
            "#ops"
        );
        assert_eq!(
            SlackChannelSelector::ChannelId("C123ABC456".into()).display_label(),
            "C123ABC456"
        );
    }

    #[tokio::test]
    async fn poll_preserves_unread_history_when_backfill_hits_max_pages() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let state = MockState::default();
        let app = Router::new().route(
            "/api/conversations.history",
            get({
                let state = state.clone();
                move |Query(query): Query<HashMap<String, String>>, headers: HeaderMap| {
                    let state = state.clone();
                    async move {
                        let channel = query.get("channel").cloned().unwrap_or_default();
                        let cursor = query.get("cursor").cloned();
                        let oldest = query.get("oldest").cloned().unwrap_or_default();
                        let latest = query.get("latest").cloned();
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        state
                            .push_hit(format!(
                                "history channel={channel} cursor={cursor:?} oldest={oldest} latest={latest:?} auth={auth}"
                            ))
                            .await;

                        let body = match (cursor.as_deref(), latest.as_deref()) {
                            (None, None) => json!({
                                "ok": true,
                                "messages": [
                                    { "ts": "1700000006.000000", "text": "m6" },
                                    { "ts": "1700000005.000000", "text": "m5" }
                                ],
                                "has_more": true,
                                "response_metadata": { "next_cursor": "cursor-2" }
                            }),
                            (Some("cursor-2"), None) => json!({
                                "ok": true,
                                "messages": [
                                    { "ts": "1700000004.000000", "text": "m4" },
                                    { "ts": "1700000003.000000", "text": "m3" }
                                ],
                                "has_more": true,
                                "response_metadata": { "next_cursor": "cursor-3" }
                            }),
                            (Some("cursor-3"), None) => json!({
                                "ok": true,
                                "messages": [
                                    { "ts": "1700000002.000000", "text": "m2" },
                                    { "ts": "1700000001.000000", "text": "m1" }
                                ],
                                "has_more": true,
                                "response_metadata": { "next_cursor": "cursor-4" }
                            }),
                            (None, Some("1700000001.000000")) => json!({
                                "ok": true,
                                "messages": [
                                    { "ts": "1699999999.000000", "text": "older-2" },
                                    { "ts": "1699999998.000000", "text": "older-1" }
                                ],
                                "has_more": false,
                                "response_metadata": { "next_cursor": "" }
                            }),
                            other => panic!("unexpected history request shape: {other:?}"),
                        };

                        Json(body)
                    }
                }
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);

        let producer = SlackEventProducer::new(
            SlackReadClient::new(),
            SlackChannelSelector::ChannelId("C123ABC456".into()),
            "support/slack:id:C123ABC456".into(),
            Some("C123ABC456".into()),
        )
        .expect("build polling producer");

        let first = producer
            .poll(&ProducerCheckpoint::none())
            .await
            .expect("first poll should succeed");
        assert_eq!(first.events.len(), 1);
        let first_state = SlackCheckpoint::from_producer_checkpoint(&first.checkpoint);
        assert_eq!(first_state.last_seen_ts, None);
        assert_eq!(
            first_state.pending_last_seen_ts.as_deref(),
            Some("1700000006.000000")
        );
        assert_eq!(
            first_state.backfill_latest_ts.as_deref(),
            Some("1700000001.000000")
        );

        let second = producer
            .poll(&first.checkpoint)
            .await
            .expect("second poll should succeed");
        assert_eq!(second.events.len(), 1);
        let second_state = SlackCheckpoint::from_producer_checkpoint(&second.checkpoint);
        assert_eq!(
            second_state.last_seen_ts.as_deref(),
            Some("1700000006.000000")
        );
        assert_eq!(second_state.pending_last_seen_ts, None);
        assert_eq!(second_state.backfill_latest_ts, None);

        let hits = state.snapshot().await;
        assert!(
            hits.iter()
                .any(|hit| hit.contains("latest=Some(\"1700000001.000000\")")),
            "expected second poll to continue the backfill window, hits: {hits:?}"
        );
    }

    #[tokio::test]
    async fn poll_re_resolves_channel_name_after_stale_cached_id() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let state = MockState::default();
        let app = Router::new()
            .route(
                "/api/conversations.list",
                get({
                    let state = state.clone();
                    move |Query(query): Query<HashMap<String, String>>| {
                        let state = state.clone();
                        async move {
                            let cursor = query.get("cursor").cloned();
                            state.push_hit(format!("list cursor={cursor:?}")).await;
                            Json(json!({
                                "ok": true,
                                "channels": [
                                    { "id": "CNEW12345", "name": "ops" }
                                ],
                                "response_metadata": { "next_cursor": "" }
                            }))
                        }
                    }
                }),
            )
            .route(
                "/api/conversations.history",
                get({
                    let state = state.clone();
                    move |Query(query): Query<HashMap<String, String>>| {
                        let state = state.clone();
                        async move {
                            let channel = query.get("channel").cloned().unwrap_or_default();
                            state.push_hit(format!("history channel={channel}")).await;
                            let body = if channel == "COLD12345" {
                                json!({
                                    "ok": false,
                                    "error": "channel_not_found"
                                })
                            } else if channel == "CNEW12345" {
                                json!({
                                    "ok": true,
                                    "messages": [
                                        { "ts": "1700000001.000000", "text": "fresh" }
                                    ],
                                    "has_more": false,
                                    "response_metadata": { "next_cursor": "" }
                                })
                            } else {
                                panic!("unexpected channel requested: {channel}");
                            };
                            Json(body)
                        }
                    }
                }),
            );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);

        let producer = SlackEventProducer::new(
            SlackReadClient::new(),
            SlackChannelSelector::ChannelName("ops".into()),
            "support/slack:name:ops".into(),
            Some("COLD12345".into()),
        )
        .expect("build name-based producer");
        assert_eq!(producer.producer_key(), "support/slack:name:ops");
        producer.set_cached_channel_id("COLD12345".into()).await;

        let checkpoint = SlackCheckpoint {
            last_seen_ts: None,
            pending_last_seen_ts: None,
            backfill_latest_ts: None,
            resolved_channel_id: Some("COLD12345".into()),
        }
        .to_producer_checkpoint();
        let poll = producer
            .poll(&checkpoint)
            .await
            .expect("poll should recover from stale channel id");

        assert_eq!(poll.events.len(), 1);
        let state_after = SlackCheckpoint::from_producer_checkpoint(&poll.checkpoint);
        assert_eq!(
            state_after.resolved_channel_id.as_deref(),
            Some("CNEW12345")
        );
        assert_eq!(
            state_after.last_seen_ts.as_deref(),
            Some("1700000001.000000")
        );

        let hits = state.snapshot().await;
        assert_eq!(
            hits,
            vec![
                "history channel=COLD12345".to_string(),
                "list cursor=None".to_string(),
                "history channel=CNEW12345".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn poll_keeps_backfill_open_when_page_only_contains_already_seen_messages() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let state = MockState::default();
        let app = Router::new().route(
            "/api/conversations.history",
            get({
                let state = state.clone();
                move |Query(query): Query<HashMap<String, String>>| {
                    let state = state.clone();
                    async move {
                        let latest = query.get("latest").cloned();
                        state.push_hit(format!("history latest={latest:?}")).await;
                        Json(json!({
                            "ok": true,
                            "messages": [
                                { "ts": "1700000004.000000", "text": "already-seen-4" },
                                { "ts": "1700000003.000000", "text": "already-seen-3" }
                            ],
                            "has_more": true,
                            "response_metadata": { "next_cursor": "cursor-2" }
                        }))
                    }
                }
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);

        let producer = SlackEventProducer::new(
            SlackReadClient::new(),
            SlackChannelSelector::ChannelId("C123ABC456".into()),
            "support/slack:id:C123ABC456".into(),
            Some("C123ABC456".into()),
        )
        .expect("build polling producer");

        let checkpoint = SlackCheckpoint {
            last_seen_ts: Some("1700000004.000000".into()),
            pending_last_seen_ts: Some("1700000006.000000".into()),
            backfill_latest_ts: Some("1700000005.000000".into()),
            resolved_channel_id: Some("C123ABC456".into()),
        }
        .to_producer_checkpoint();
        let poll = producer
            .poll(&checkpoint)
            .await
            .expect("poll should preserve partial backfill state");

        assert!(
            poll.events.is_empty(),
            "page should dedupe to no new events"
        );
        let state_after = SlackCheckpoint::from_producer_checkpoint(&poll.checkpoint);
        assert_eq!(
            state_after.last_seen_ts.as_deref(),
            Some("1700000004.000000")
        );
        assert_eq!(
            state_after.pending_last_seen_ts.as_deref(),
            Some("1700000006.000000")
        );
        assert_eq!(
            state_after.backfill_latest_ts.as_deref(),
            Some("1700000003.000000")
        );
        assert_eq!(
            state.snapshot().await,
            vec![
                "history latest=Some(\"1700000005.000000\")".to_string(),
                "history latest=Some(\"1700000005.000000\")".to_string(),
                "history latest=Some(\"1700000005.000000\")".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn poll_enqueues_normalized_batch_when_ingress_store_is_installed() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let (_store_guard, store) = install_memory_ingress_store();
        let app = Router::new().route(
            "/api/conversations.history",
            get(|| async move {
                Json(json!({
                    "ok": true,
                    "messages": [
                        {
                            "type": "message",
                            "user": "U123",
                            "text": "Track the OAuth docs follow-up.",
                            "ts": "1700000001.000001",
                            "thread_ts": "1700000001.000001",
                            "reply_count": 1,
                            "latest_reply": "1700000002.000001"
                        }
                    ],
                    "has_more": false,
                    "response_metadata": { "next_cursor": "" }
                }))
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);

        let producer = SlackEventProducer::new(
            SlackReadClient::new(),
            SlackChannelSelector::ChannelId("C123ABC456".into()),
            "support/slack:id:C123ABC456".into(),
            Some("C123ABC456".into()),
        )
        .expect("build polling producer");

        let poll = producer
            .poll(&ProducerCheckpoint::none())
            .await
            .expect("poll should enqueue to the durable inbox");

        assert!(
            poll.events.is_empty(),
            "polling receiver should enqueue instead of delivering directly when the inbox store is installed"
        );
        let checkpoint = SlackCheckpoint::from_producer_checkpoint(&poll.checkpoint);
        assert_eq!(
            checkpoint.last_seen_ts.as_deref(),
            Some("1700000001.000001")
        );

        let pending_items = store.pending_items().await;
        assert_eq!(pending_items.len(), 1, "expected one durable inbox item");
        let item = &pending_items[0];
        assert!(
            item.ingress_id
                .as_str()
                .starts_with("support/slack:poll:slack:C123ABC456:"),
            "expected deterministic polling ingress id, got {}",
            item.ingress_id
        );
        assert_eq!(item.source_key.as_str(), "slack:C123ABC456");
        let batch: SlackNormalizedBatch =
            serde_json::from_str(&item.payload_json).expect("deserialize batch");
        assert_eq!(batch.source.source_kind, "slack");
        assert_eq!(batch.source.source_key.as_str(), "slack:C123ABC456");
        assert_eq!(batch.source.source_label, "C123ABC456");
        assert_eq!(
            batch.transport.as_ref().map(|transport| &transport.kind),
            Some(&crate::normalize::SlackTransportKind::Polling)
        );
        assert_eq!(batch.records.len(), 1);
        assert_eq!(
            batch.records[0].source_ref(),
            Some("slack://channel/C123ABC456/p1700000001000001")
        );
        assert_eq!(
            batch.records[0].text(),
            Some("Track the OAuth docs follow-up.")
        );
        assert_eq!(batch.records[0].user(), None);
        assert_eq!(
            batch.records[0].raw(),
            &json!({
                "type": "message",
                "user": "U123",
                "text": "Track the OAuth docs follow-up.",
                "ts": "1700000001.000001",
                "thread_ts": "1700000001.000001",
                "reply_count": 1,
                "latest_reply": "1700000002.000001"
            })
        );
    }

    #[tokio::test]
    async fn inbox_producer_reconciles_delivery_on_the_next_poll() {
        let _guard = crate::slack_test_env_lock().lock().await;
        let (_store_guard, store) = install_memory_ingress_store();
        let source_key = polling_source_key("C123ABC456").expect("valid source key");
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
            ingress_id: polling_ingress_id(&source_key, &batch_messages).expect("valid ingress id"),
            source_key: source_key.clone(),
            payload_json: serde_json::to_string(&normalized_batch).expect("serialize batch"),
            enqueued_at_unix_ms: 1_700_000_100_000,
        };
        store
            .enqueue(&ingress_item)
            .await
            .expect("enqueue sample durable ingress item");

        let producer = SlackInboxEventProducer::new().expect("build inbox producer");
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
        let source_key = polling_source_key("C123ABC456").expect("valid source key");
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
            ingress_id: polling_ingress_id(&source_key, &batch_messages).expect("valid ingress id"),
            source_key: source_key.clone(),
            payload_json: serde_json::to_string(&normalized_batch).expect("serialize batch"),
            enqueued_at_unix_ms: 1_700_000_100_000,
        };
        store
            .enqueue(&ingress_item)
            .await
            .expect("enqueue sample durable ingress item");

        let producer = SlackInboxEventProducer::new().expect("build inbox producer");
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
    async fn build_dedupes_name_and_id_for_same_channel() {
        use baml_rt_tools::{InventoryCatalog, ToolCatalog, ToolName};

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
                            "ok": true,
                            "channels": [
                                { "id": "C123ABC456", "name": "ops" }
                            ],
                            "response_metadata": { "next_cursor": "" }
                        }))
                    }
                }
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);
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
        })
        .await
        .expect("producer build should succeed");

        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].producer_key(), "support/slack:name:ops");
        assert_eq!(state.snapshot().await, vec!["list".to_string()]);
    }

    #[tokio::test]
    async fn build_resolves_channel_names_across_all_pages() {
        use baml_rt_tools::{InventoryCatalog, ToolCatalog, ToolName};

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
        let metadata = InventoryCatalog::new()
            .by_name(&ToolName::parse("support/slack").expect("valid tool name"))
            .cloned()
            .expect("support/slack metadata");

        let producers = build_slack_event_producers(EventProducerBuildContext {
            metadata,
            config: Some(json!({
                "channels": ["ops"]
            })),
            persisted_checkpoints: Arc::new(HashMap::new()),
        })
        .await
        .expect("producer build should succeed");

        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].producer_key(), "support/slack:name:ops");
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
    async fn build_reuses_persisted_name_checkpoint_without_rerunning_conversations_list() {
        use baml_rt_tools::{InventoryCatalog, ToolCatalog, ToolName};

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
        let metadata = InventoryCatalog::new()
            .by_name(&ToolName::parse("support/slack").expect("valid tool name"))
            .cloned()
            .expect("support/slack metadata");

        let mut persisted_checkpoints = HashMap::new();
        persisted_checkpoints.insert(
            "support/slack:name:ops".to_string(),
            persisted_checkpoint_for_channel_id("C123ABC456"),
        );

        let producers = build_slack_event_producers(EventProducerBuildContext {
            metadata,
            config: Some(json!({
                "channels": ["ops"]
            })),
            persisted_checkpoints: Arc::new(persisted_checkpoints),
        })
        .await
        .expect("producer build should reuse persisted resolved channel id");

        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].producer_key(), "support/slack:name:ops");
        assert!(
            state.snapshot().await.is_empty(),
            "expected persisted restart to avoid conversations.list"
        );
    }

    #[tokio::test]
    async fn build_reuses_persisted_checkpoint_across_name_to_id_switch() {
        use baml_rt_tools::{InventoryCatalog, ToolCatalog, ToolName};

        let _guard = crate::slack_test_env_lock().lock().await;
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let metadata = InventoryCatalog::new()
            .by_name(&ToolName::parse("support/slack").expect("valid tool name"))
            .cloned()
            .expect("support/slack metadata");

        let mut persisted_checkpoints = HashMap::new();
        persisted_checkpoints.insert(
            "support/slack:name:ops".to_string(),
            persisted_checkpoint_for_channel_id("C123ABC456"),
        );

        let producers = build_slack_event_producers(EventProducerBuildContext {
            metadata,
            config: Some(json!({
                "channels": ["C123ABC456"]
            })),
            persisted_checkpoints: Arc::new(persisted_checkpoints),
        })
        .await
        .expect("producer build should reuse persisted checkpoint identity");

        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].producer_key(), "support/slack:name:ops");
    }

    #[tokio::test]
    async fn build_registers_inbox_producer_without_polling_channels() {
        use baml_rt_tools::{InventoryCatalog, ToolCatalog, ToolName};

        let _guard = crate::slack_test_env_lock().lock().await;
        let (_store_guard, _store) = install_memory_ingress_store();
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
        })
        .await
        .expect("producer build should still register the durable inbox producer");

        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].producer_key(), "support/slack:inbox");
    }
}
