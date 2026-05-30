// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Slack Socket Mode WebSocket receiver.
//!
//! Maintains a persistent WebSocket connection to Slack via Socket Mode,
//! receives real-time events, normalizes supported message events, and
//! durably enqueues them through [`IngressStore`]. The existing
//! `support/slack:inbox` producer drains the store into the dispatch path.

use std::{collections::HashMap, sync::Arc, time::Duration};

use baml_rt_core::{
    BamlRtError, ExponentialBackoff, Result, clock_events,
    event_subscription::EventSourceKey,
    ingress_store::{IngressId, IngressItem, IngressStore},
    time::{now_unix_ms, now_unix_secs},
};
use futures_util::{SinkExt, StreamExt};
use integrations_slack_read::{SlackAuthPreference, SlackReadClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    normalize::{SocketModeEventContext, normalize_socket_mode_batch},
    producer::{RAW_SOURCE_SCHEMA_VERSION, SlackChannelSelector, resolve_selector_channel_id},
};

/// Read timeout for the Socket Mode WebSocket stream. Slack sends pings
/// approximately every 30 seconds; 90s allows three missed pings before
/// treating the connection as dead.
const WS_READ_TIMEOUT: Duration = Duration::from_secs(90);

// ---------------------------------------------------------------------------
// Envelope types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum SocketEnvelope {
    Hello {
        #[serde(default)]
        num_connections: u32,
    },
    Disconnect {
        reason: String,
    },
    EventsApi {
        envelope_id: String,
        payload: Box<EventsApiPayload>,
        #[serde(default)]
        retry_attempt: u32,
        #[serde(default)]
        retry_reason: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EventsApiPayload {
    pub event: Value,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub api_app_id: Option<String>,
    #[serde(default)]
    pub event_context: Option<String>,
    #[serde(default)]
    pub authorizations: Vec<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EnvelopeAck {
    envelope_id: String,
}

// ---------------------------------------------------------------------------
// Channel allowlist entry
// ---------------------------------------------------------------------------

struct SocketModeChannel {
    source_key: EventSourceKey,
    source_label: String,
}

// ---------------------------------------------------------------------------
// Disconnect reason (controls backoff vs immediate reconnect)
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum DisconnectReason {
    /// Slack asked us to reconnect (e.g. `refresh_requested`, `warning`). No backoff.
    ServerRequested,
    /// App link was permanently disabled; do not reconnect.
    LinkDisabled,
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

pub(crate) struct SocketModeReceiver {
    client: SlackReadClient,
    app_token: String,
    channels: HashMap<String, SocketModeChannel>,
    store: Arc<dyn IngressStore>,
    cancel: CancellationToken,
}

impl SocketModeReceiver {
    /// Run the receive loop. Reconnects on disconnect/error with backoff.
    /// Exits when the [`CancellationToken`] is cancelled or the Slack link is
    /// permanently disabled.
    async fn run(self) {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_millis(500), Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    info!("Socket Mode receiver cancelled; shutting down");
                    break;
                }
                result = self.connect_and_receive() => {
                    match result {
                        Ok(DisconnectReason::ServerRequested) => {
                            info!("Socket Mode server requested disconnect; reconnecting");
                            backoff.reset();
                        }
                        Ok(DisconnectReason::LinkDisabled) => {
                            warn!("Socket Mode app link disabled; stopping receiver");
                            break;
                        }
                        Err(err) => {
                            let delay = backoff.next_delay();
                            warn!(
                                error = %err,
                                backoff_ms = delay.as_millis() as u64,
                                "Socket Mode connection error; reconnecting after backoff"
                            );
                            tokio::time::sleep(delay).await;
                        }
                    }
                }
            }
        }
    }

    /// Single connection lifecycle: open URL, connect WS, process envelopes.
    async fn connect_and_receive(&self) -> Result<DisconnectReason> {
        let ws_url = self.open_connection().await?;
        info!(url = %ws_url, "Socket Mode WebSocket connecting");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|err| {
                BamlRtError::ToolExecution(format!("WebSocket connect failed: {err}"))
            })?;

        let (mut write, mut read) = ws_stream.split();

        loop {
            match tokio::time::timeout(WS_READ_TIMEOUT, read.next()).await {
                Err(_elapsed) => {
                    return Err(BamlRtError::ToolExecution(
                        "Socket Mode WebSocket read timed out (no data for 90s)".to_string(),
                    ));
                }
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Some(reason) = self.handle_text_message(&text, &mut write).await? {
                        return Ok(reason);
                    }
                }
                Ok(Some(Ok(Message::Ping(payload)))) => {
                    if let Err(err) = write.send(Message::Pong(payload)).await {
                        warn!(error = %err, "Socket Mode pong send failed");
                        return Err(BamlRtError::ToolExecution(format!(
                            "WebSocket pong write failed: {err}"
                        )));
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) => {
                    info!("Socket Mode WebSocket closed by server");
                    return Ok(DisconnectReason::ServerRequested);
                }
                Ok(None) => {
                    return Err(BamlRtError::ToolExecution(
                        "Socket Mode WebSocket stream ended unexpectedly".to_string(),
                    ));
                }
                Ok(Some(Err(err))) => {
                    return Err(BamlRtError::ToolExecution(format!(
                        "WebSocket read error: {err}"
                    )));
                }
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    /// Call `apps.connections.open` to obtain a WebSocket URL.
    async fn open_connection(&self) -> Result<String> {
        let json = self
            .client
            .post_json("apps.connections.open", &self.app_token)
            .await
            .map_err(|err| {
                BamlRtError::ToolExecution(format!("apps.connections.open failed: {err}"))
            })?;
        json.get("url")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| {
                BamlRtError::ToolExecution(
                    "apps.connections.open response missing 'url' field".to_string(),
                )
            })
    }

    /// Returns `Ok(Some(reason))` when a disconnect envelope is received, signalling
    /// `connect_and_receive` to exit the message loop. Returns `Ok(None)` for all
    /// other handled envelopes.
    async fn handle_text_message<S>(
        &self,
        text: &str,
        write: &mut S,
    ) -> Result<Option<DisconnectReason>>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::fmt::Display,
    {
        let envelope: SocketEnvelope = match serde_json::from_str(text) {
            Ok(env) => env,
            Err(err) => {
                warn!(error = %err, "failed to parse Socket Mode envelope; ignoring");
                return Ok(None);
            }
        };

        match envelope {
            SocketEnvelope::Hello { num_connections } => {
                info!(num_connections, "Socket Mode hello received");
            }
            SocketEnvelope::Disconnect { reason } => {
                info!(reason = %reason, "Socket Mode disconnect requested");
                if reason == "link_disabled" {
                    return Ok(Some(DisconnectReason::LinkDisabled));
                }
                return Ok(Some(DisconnectReason::ServerRequested));
            }
            SocketEnvelope::EventsApi {
                envelope_id,
                payload,
                retry_attempt,
                retry_reason,
            } => {
                self.handle_events_api(
                    &envelope_id,
                    &payload,
                    retry_attempt,
                    retry_reason.as_deref(),
                    write,
                )
                .await?;
            }
            SocketEnvelope::Unknown => {
                // NOTE: `#[serde(other)]` discards all fields from
                // unrecognised envelope types, so we re-parse as `Value` to
                // extract `envelope_id` for acking. The simpler enum design
                // is worth this rare-path double-parse.
                debug!("ignoring unknown Socket Mode envelope type");
                if let Ok(raw) = serde_json::from_str::<Value>(text)
                    && let Some(id) = raw.get("envelope_id").and_then(Value::as_str)
                {
                    Self::ack(write, id).await?;
                }
            }
        }
        Ok(None)
    }

    async fn handle_events_api<S>(
        &self,
        envelope_id: &str,
        payload: &EventsApiPayload,
        retry_attempt: u32,
        retry_reason: Option<&str>,
        write: &mut S,
    ) -> Result<()>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::fmt::Display,
    {
        // 1. Require event_id for durable dedupe.
        let event_id = match &payload.event_id {
            Some(id) if !id.is_empty() => id.as_str(),
            _ => {
                warn!(
                    envelope_id,
                    "Socket Mode event missing event_id; acking without enqueue"
                );
                Self::ack(write, envelope_id).await?;
                return Ok(());
            }
        };

        // 2. Extract channel from inner event.
        let channel_id = match payload.event.get("channel").and_then(Value::as_str) {
            Some(ch) => ch,
            None => {
                debug!(
                    envelope_id,
                    event_id, "Socket Mode event has no channel; acking without enqueue"
                );
                Self::ack(write, envelope_id).await?;
                return Ok(());
            }
        };

        // 3. Channel allowlist filter.
        let channel_meta = match self.channels.get(channel_id) {
            Some(meta) => meta,
            None => {
                debug!(
                    channel = %channel_id,
                    event_id,
                    "Socket Mode event filtered (channel not in allowlist)"
                );
                Self::ack(write, envelope_id).await?;
                return Ok(());
            }
        };

        // 4. Normalize.
        let emitted_at = now_unix_secs(clock_events::SOCKET_MODE_NORMALIZE);
        let ctx = SocketModeEventContext {
            event: &payload.event,
            event_id,
            team_id: payload.team_id.as_deref(),
            api_app_id: payload.api_app_id.as_deref(),
            event_context: payload.event_context.as_deref(),
            retry_attempt,
            retry_reason,
            authorizations: &payload.authorizations,
        };
        let batch = match normalize_socket_mode_batch(
            RAW_SOURCE_SCHEMA_VERSION,
            channel_id,
            &channel_meta.source_key,
            &channel_meta.source_label,
            &ctx,
            emitted_at,
        ) {
            Some(batch) => batch,
            None => {
                let event_type = payload
                    .event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                debug!(
                    envelope_id,
                    event_type, "Socket Mode event type not supported; acking without enqueue"
                );
                Self::ack(write, envelope_id).await?;
                return Ok(());
            }
        };

        // 5. Enqueue.
        let ingress_id =
            IngressId::parse(format!("support/slack:socket:{event_id}")).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "Socket Mode ingress ID is empty for event_id {event_id}"
                ))
            })?;
        let payload_json = serde_json::to_string(&batch).map_err(|err| {
            BamlRtError::InvalidArgument(format!(
                "failed to serialize Socket Mode ingress payload: {err}"
            ))
        })?;
        let enqueued = self
            .store
            .enqueue(&IngressItem {
                ingress_id: ingress_id.clone(),
                source_key: channel_meta.source_key.clone(),
                payload_json,
                enqueued_at_unix_ms: now_unix_ms(clock_events::SOCKET_MODE_ENQUEUE),
            })
            .await?;

        if enqueued {
            info!(
                ingress_id = %ingress_id,
                channel = %channel_id,
                event_id,
                "Socket Mode enqueue success"
            );
        } else {
            debug!(
                ingress_id = %ingress_id,
                event_id,
                "Socket Mode duplicate event_id; acking"
            );
        }

        // 6. Ack after durable enqueue.
        Self::ack(write, envelope_id).await
    }

    async fn ack<S>(write: &mut S, envelope_id: &str) -> Result<()>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::fmt::Display,
    {
        let payload = serde_json::to_string(&EnvelopeAck {
            envelope_id: envelope_id.to_string(),
        })
        .map_err(|err| {
            BamlRtError::InvalidArgument(format!("Socket Mode ack serialization failed: {err}"))
        })?;
        write
            .send(Message::Text(payload.into()))
            .await
            .map_err(|err| {
                BamlRtError::ToolExecution(format!("WebSocket ack write failed: {err}"))
            })?;
        debug!(envelope_id, "Socket Mode ack sent");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Resolve channels and spawn the Socket Mode receiver as a background task.
pub(crate) async fn start_socket_mode_receiver(
    client: SlackReadClient,
    app_token: String,
    channels: Vec<SlackChannelSelector>,
    store: Arc<dyn IngressStore>,
    cancel: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>> {
    let (token, _kind) = client
        .select_token(Some(SlackAuthPreference::Auto), false)
        .map_err(|err| {
            BamlRtError::InvalidArgument(format!(
                "Socket Mode requires SLACK_BOT_TOKEN for channel resolution: {err}"
            ))
        })?;
    let token = token.to_string();

    let mut channel_map = HashMap::new();
    for selector in &channels {
        let channel_id = resolve_selector_channel_id(&client, &token, selector)
            .await
            .map_err(|err| {
                BamlRtError::ToolExecution(format!(
                    "Socket Mode channel resolution failed for {}: {err}",
                    selector.display_label()
                ))
            })?;
        let source_key = EventSourceKey::parse(format!("slack:{channel_id}")).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!("invalid source key for channel {channel_id}"))
        })?;
        let source_label = selector.display_label();
        info!(
            channel_id = %channel_id,
            label = %source_label,
            "Socket Mode channel resolved"
        );
        channel_map.insert(
            channel_id,
            SocketModeChannel {
                source_key,
                source_label,
            },
        );
    }

    let receiver = SocketModeReceiver {
        client,
        app_token,
        channels: channel_map,
        store,
        cancel,
    };

    let handle = tokio::spawn(async move {
        receiver.run().await;
    });

    info!("Socket Mode receiver started");
    Ok(handle)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hello_envelope() {
        let raw = r#"{"type":"hello","num_connections":1}"#;
        let env: SocketEnvelope = serde_json::from_str(raw).unwrap();
        assert!(matches!(env, SocketEnvelope::Hello { num_connections: 1 }));
    }

    #[test]
    fn parse_hello_envelope_missing_num_connections() {
        let raw = r#"{"type":"hello"}"#;
        let env: SocketEnvelope = serde_json::from_str(raw).unwrap();
        assert!(matches!(env, SocketEnvelope::Hello { num_connections: 0 }));
    }

    #[test]
    fn parse_disconnect_envelope() {
        let raw = r#"{"type":"disconnect","reason":"link_disabled"}"#;
        let env: SocketEnvelope = serde_json::from_str(raw).unwrap();
        match env {
            SocketEnvelope::Disconnect { reason } => assert_eq!(reason, "link_disabled"),
            other => panic!("expected Disconnect, got {other:?}"),
        }
    }

    #[test]
    fn parse_events_api_message_envelope() {
        let raw = r#"{
            "type": "events_api",
            "envelope_id": "abc-123",
            "payload": {
                "team_id": "T01",
                "api_app_id": "A01",
                "event": {
                    "type": "message",
                    "channel": "C123",
                    "user": "U01",
                    "text": "hello",
                    "ts": "1700000001.000001"
                },
                "event_id": "Ev01ABC"
            }
        }"#;
        let env: SocketEnvelope = serde_json::from_str(raw).unwrap();
        match env {
            SocketEnvelope::EventsApi {
                envelope_id,
                payload,
                retry_attempt,
                retry_reason,
            } => {
                assert_eq!(envelope_id, "abc-123");
                assert_eq!(payload.event_id.as_deref(), Some("Ev01ABC"));
                assert_eq!(payload.team_id.as_deref(), Some("T01"));
                assert_eq!(payload.api_app_id.as_deref(), Some("A01"));
                assert_eq!(retry_attempt, 0);
                assert!(retry_reason.is_none());
                assert_eq!(
                    payload.event.get("channel").and_then(Value::as_str),
                    Some("C123")
                );
            }
            other => panic!("expected EventsApi, got {other:?}"),
        }
    }

    #[test]
    fn parse_events_api_with_retry() {
        let raw = r#"{
            "type": "events_api",
            "envelope_id": "retry-env",
            "retry_attempt": 2,
            "retry_reason": "timeout",
            "payload": {
                "event": {"type": "message", "channel": "C1", "text": "hi"},
                "event_id": "Ev02"
            }
        }"#;
        let env: SocketEnvelope = serde_json::from_str(raw).unwrap();
        match env {
            SocketEnvelope::EventsApi {
                retry_attempt,
                retry_reason,
                ..
            } => {
                assert_eq!(retry_attempt, 2);
                assert_eq!(retry_reason.as_deref(), Some("timeout"));
            }
            other => panic!("expected EventsApi, got {other:?}"),
        }
    }

    #[test]
    fn unknown_envelope_type_deserializes() {
        let raw = r#"{"type":"interactive","envelope_id":"xyz"}"#;
        let env: SocketEnvelope = serde_json::from_str(raw).unwrap();
        assert!(matches!(env, SocketEnvelope::Unknown));
    }

    #[test]
    fn ingress_id_format_for_socket_mode() {
        let id = IngressId::parse("support/slack:socket:Ev01ABC").unwrap();
        assert_eq!(id.as_str(), "support/slack:socket:Ev01ABC");
    }

    #[test]
    fn envelope_ack_serializes() {
        let ack = EnvelopeAck {
            envelope_id: "abc-123".to_string(),
        };
        let json: Value = serde_json::from_str(&serde_json::to_string(&ack).unwrap()).unwrap();
        assert_eq!(json["envelope_id"], "abc-123");
    }

    /// Collects acked envelope_ids sent through the WebSocket write half.
    struct AckCollector {
        acks: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl AckCollector {
        fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
            let acks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (Self { acks: acks.clone() }, acks)
        }
    }

    impl futures_util::Sink<Message> for AckCollector {
        type Error = tokio_tungstenite::tungstenite::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn start_send(
            self: std::pin::Pin<&mut Self>,
            item: Message,
        ) -> std::result::Result<(), Self::Error> {
            if let Message::Text(text) = item
                && let Ok(ack) = serde_json::from_str::<EnvelopeAck>(&text)
            {
                self.acks.lock().unwrap().push(ack.envelope_id);
            }
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn make_test_receiver(store: std::sync::Arc<dyn IngressStore>) -> SocketModeReceiver {
        let mut channels = HashMap::new();
        channels.insert(
            "C123TEST".to_string(),
            SocketModeChannel {
                source_key: EventSourceKey::parse("slack:C123TEST").unwrap(),
                source_label: "#test".to_string(),
            },
        );
        SocketModeReceiver {
            client: SlackReadClient::new(),
            app_token: String::new(),
            channels,
            store,
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn handle_events_api_enqueues_and_acks() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let payload = EventsApiPayload {
            event: serde_json::json!({
                "type": "message",
                "channel": "C123TEST",
                "user": "U01",
                "text": "hello world",
                "ts": "1700000001.000001"
            }),
            event_id: Some("Ev100".to_string()),
            team_id: Some("T01".to_string()),
            api_app_id: None,
            event_context: None,
            authorizations: vec![],
        };

        receiver
            .handle_events_api("env-001", &payload, 0, None, &mut sink)
            .await
            .unwrap();

        // Verify enqueue
        let pending = store.list_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].ingress_id.as_str(), "support/slack:socket:Ev100");
        assert_eq!(pending[0].source_key.as_str(), "slack:C123TEST");

        // Verify ack
        let acked = acks.lock().unwrap();
        assert_eq!(&*acked, &["env-001"]);

        // Verify payload is valid normalized batch
        let batch: crate::SlackNormalizedBatch =
            serde_json::from_str(&pending[0].payload_json).unwrap();
        assert_eq!(batch.schema_version, "host.source-records.v1");
        let transport = batch.transport.unwrap();
        assert_eq!(
            transport.kind,
            crate::normalize::SlackTransportKind::SocketMode
        );
        assert_eq!(transport.delivery_id.as_deref(), Some("Ev100"));
    }

    #[tokio::test]
    async fn handle_events_api_deduplicates_event_id() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let payload = EventsApiPayload {
            event: serde_json::json!({
                "type": "message",
                "channel": "C123TEST",
                "text": "hello",
                "ts": "1700000001.000001"
            }),
            event_id: Some("EvDUPE".to_string()),
            team_id: None,
            api_app_id: None,
            event_context: None,
            authorizations: vec![],
        };

        // First delivery
        receiver
            .handle_events_api("env-first", &payload, 0, None, &mut sink)
            .await
            .unwrap();
        // Duplicate delivery
        receiver
            .handle_events_api("env-second", &payload, 1, Some("timeout"), &mut sink)
            .await
            .unwrap();

        // Only one item in store
        let pending = store.list_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);

        // Both envelopes were acked
        let acked = acks.lock().unwrap();
        assert_eq!(acked.len(), 2);
    }

    #[tokio::test]
    async fn handle_events_api_filters_unconfigured_channel() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let payload = EventsApiPayload {
            event: serde_json::json!({
                "type": "message",
                "channel": "C_NOT_ALLOWED",
                "text": "should be filtered"
            }),
            event_id: Some("EvFiltered".to_string()),
            team_id: None,
            api_app_id: None,
            event_context: None,
            authorizations: vec![],
        };

        receiver
            .handle_events_api("env-filtered", &payload, 0, None, &mut sink)
            .await
            .unwrap();

        // Nothing enqueued
        let pending = store.list_pending(10).await.unwrap();
        assert!(pending.is_empty());

        // Ack was still sent
        let acked = acks.lock().unwrap();
        assert_eq!(&*acked, &["env-filtered"]);
    }

    #[tokio::test]
    async fn handle_events_api_skips_missing_event_id() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let payload = EventsApiPayload {
            event: serde_json::json!({
                "type": "message",
                "channel": "C123TEST",
                "text": "no event_id"
            }),
            event_id: None,
            team_id: None,
            api_app_id: None,
            event_context: None,
            authorizations: vec![],
        };

        receiver
            .handle_events_api("env-no-id", &payload, 0, None, &mut sink)
            .await
            .unwrap();

        let pending = store.list_pending(10).await.unwrap();
        assert!(pending.is_empty());

        let acked = acks.lock().unwrap();
        assert_eq!(&*acked, &["env-no-id"]);
    }

    #[tokio::test]
    async fn handle_events_api_skips_non_message_events() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let payload = EventsApiPayload {
            event: serde_json::json!({
                "type": "reaction_added",
                "channel": "C123TEST"
            }),
            event_id: Some("EvReaction".to_string()),
            team_id: None,
            api_app_id: None,
            event_context: None,
            authorizations: vec![],
        };

        receiver
            .handle_events_api("env-reaction", &payload, 0, None, &mut sink)
            .await
            .unwrap();

        let pending = store.list_pending(10).await.unwrap();
        assert!(pending.is_empty());

        let acked = acks.lock().unwrap();
        assert_eq!(&*acked, &["env-reaction"]);
    }

    // ---------------------------------------------------------------------------
    // handle_text_message dispatch routing
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn handle_text_message_events_api_enqueues_and_acks() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let text = r#"{
            "type": "events_api",
            "envelope_id": "env-txt-001",
            "payload": {
                "event": {
                    "type": "message",
                    "channel": "C123TEST",
                    "user": "U01",
                    "text": "hello from text",
                    "ts": "1700000001.000001"
                },
                "event_id": "EvTXT001"
            }
        }"#;

        let result = receiver.handle_text_message(text, &mut sink).await.unwrap();
        assert!(result.is_none(), "events_api should not signal disconnect");

        {
            let acked = acks.lock().unwrap();
            assert_eq!(&*acked, &["env-txt-001"]);
        }

        let pending = store.list_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].ingress_id.as_str(),
            "support/slack:socket:EvTXT001"
        );
    }

    #[tokio::test]
    async fn handle_text_message_hello_returns_none_no_ack() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let text = r#"{"type":"hello","num_connections":1}"#;
        let result = receiver.handle_text_message(text, &mut sink).await.unwrap();
        assert!(result.is_none());
        assert!(acks.lock().unwrap().is_empty(), "hello should not ack");
        assert!(store.list_pending(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn handle_text_message_disconnect_link_disabled_signals_reason() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, _acks) = AckCollector::new();

        let text = r#"{"type":"disconnect","reason":"link_disabled"}"#;
        let result = receiver.handle_text_message(text, &mut sink).await.unwrap();
        assert!(
            matches!(result, Some(DisconnectReason::LinkDisabled)),
            "expected LinkDisabled, got {result:?}"
        );
    }

    #[tokio::test]
    async fn handle_text_message_disconnect_refresh_requested_signals_server_requested() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, _acks) = AckCollector::new();

        let text = r#"{"type":"disconnect","reason":"refresh_requested"}"#;
        let result = receiver.handle_text_message(text, &mut sink).await.unwrap();
        assert!(
            matches!(result, Some(DisconnectReason::ServerRequested)),
            "expected ServerRequested, got {result:?}"
        );
    }

    #[tokio::test]
    async fn handle_text_message_unknown_type_with_envelope_id_acks() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let text = r#"{"type":"interactive","envelope_id":"env-unknown-001"}"#;
        let result = receiver.handle_text_message(text, &mut sink).await.unwrap();
        assert!(result.is_none());
        assert_eq!(&*acks.lock().unwrap(), &["env-unknown-001"]);
    }

    #[tokio::test]
    async fn handle_text_message_malformed_json_returns_none_no_ack() {
        let (_guard, store) =
            baml_rt_tools::ingress_store::test_support::install_memory_ingress_store();
        let receiver = make_test_receiver(store.clone() as Arc<dyn IngressStore>);
        let (mut sink, acks) = AckCollector::new();

        let result = receiver
            .handle_text_message("this is not json at all", &mut sink)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "malformed envelope should not signal disconnect"
        );
        assert!(
            acks.lock().unwrap().is_empty(),
            "malformed envelope should not ack"
        );
    }
}
