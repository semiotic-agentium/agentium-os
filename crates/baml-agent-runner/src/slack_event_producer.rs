//! Slack event producer: polls `conversations.history` and delivers raw
//! messages to subscribed agents via the event dispatcher.
//!
//! Agents own interpretation — this producer delivers raw Slack messages
//! as event payloads. Configuration is via CLI flag `--slack-event-channel`.

use async_trait::async_trait;
use baml_rt_core::{
    AgentDispatchRoutingKey, BamlRtError, EventSchemaVersion, EventSourceKind, ProducedEvent,
    Result, event_subscription::EventSourceKey,
};
use baml_rt_tools::{EventProducer, ProducerCheckpoint, ProducerPoll};
use integrations_slack_read::{SlackAuthPreference, SlackReadClient};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, info};

/// Schema version for raw Slack message events.
const SCHEMA_VERSION: &str = "slack.messages.v1";

/// Default lookback window on first poll (24 hours).
const INITIAL_LOOKBACK_SECS: u64 = 86_400;

/// Maximum messages per API call.
const HISTORY_LIMIT: u16 = 200;

/// Maximum pages to fetch per poll cycle (bounds API calls per interval).
const MAX_PAGES: usize = 3;

/// Maximum pages when resolving a channel name → ID via conversations.list.
const MAX_RESOLVE_PAGES: usize = 3;

/// Serialized into the opaque [`ProducerCheckpoint`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SlackCheckpoint {
    /// Slack timestamp of the latest seen message (exclusive lower bound for next poll).
    last_seen_ts: Option<String>,
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

/// Polls a single Slack channel for new messages and emits them as
/// [`ProducedEvent`]s with schema `slack.messages.v1`.
pub struct SlackEventProducer {
    client: SlackReadClient,
    channel: String,
    producer_key: String,
    source_kinds: Vec<EventSourceKind>,
    /// Cached channel ID resolved from name. Survives across poll cycles without
    /// needing checkpoint persistence — avoids re-calling conversations.list on
    /// quiet channels where the checkpoint doesn't advance.
    resolved_channel_id: tokio::sync::OnceCell<String>,
}

impl SlackEventProducer {
    /// Create a new producer for the given channel (name or ID).
    pub fn new(channel: String) -> Result<Self> {
        let client = SlackReadClient::new();
        // Validate that at least one token is available at construction time.
        client
            .select_token(Some(SlackAuthPreference::Auto), false)
            .map_err(|err| {
                BamlRtError::InvalidArgument(format!(
                    "Slack event producer requires SLACK_BOT_TOKEN or SLACK_USER_TOKEN: {err}"
                ))
            })?;
        let producer_key = format!("slack:{channel}");
        Ok(Self {
            client,
            channel,
            producer_key,
            source_kinds: vec![
                EventSourceKind::parse("slack").expect("slack is a valid source kind"),
            ],
            resolved_channel_id: tokio::sync::OnceCell::new(),
        })
    }

    /// Resolve the channel name to an ID. Checks the in-memory cache first
    /// (survives across polls), then the checkpoint (survives across restarts),
    /// then calls conversations.list as a last resort.
    async fn resolve_channel_id(
        &self,
        token: &str,
        checkpoint: &SlackCheckpoint,
    ) -> Result<String> {
        // In-memory cache (survives across polls even when checkpoint doesn't advance).
        if let Some(id) = self.resolved_channel_id.get() {
            return Ok(id.clone());
        }
        // Checkpoint cache (survives across restarts).
        if let Some(ref id) = checkpoint.resolved_channel_id {
            // OnceCell already set is expected on subsequent polls — not an error.
            let _ = self.resolved_channel_id.set(id.clone());
            return Ok(id.clone());
        }
        // Slack IDs: start with C/D/G, followed by alphanumeric, typically 9-11 chars.
        // Safe to slice at [1..]: starts_with guard above confirms first char is ASCII.
        if self.channel.starts_with(['C', 'D', 'G'])
            && self.channel.len() >= 9
            && self.channel[1..].chars().all(|c| c.is_ascii_alphanumeric())
        {
            let _ = self.resolved_channel_id.set(self.channel.clone());
            return Ok(self.channel.clone());
        }
        // Otherwise resolve name → ID via conversations.list.
        let target = self
            .channel
            .strip_prefix('#')
            .unwrap_or(&self.channel)
            .to_lowercase();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_RESOLVE_PAGES {
            let mut query = vec![
                ("types", "public_channel,private_channel".to_string()),
                ("exclude_archived", "true".to_string()),
                ("limit", "200".to_string()),
            ];
            if let Some(ref c) = cursor {
                query.push(("cursor", c.clone()));
            }
            let json = self
                .client
                .get_json("conversations.list", token, &query)
                .await
                .map_err(|err| {
                    BamlRtError::ToolExecution(format!("Slack conversations.list failed: {err}"))
                })?;
            if let Some(channels) = json.get("channels").and_then(|c| c.as_array()) {
                for ch in channels {
                    let name = ch.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    if name.eq_ignore_ascii_case(&target)
                        && let Some(id) = ch.get("id").and_then(|i| i.as_str())
                    {
                        let id = id.to_string();
                        let _ = self.resolved_channel_id.set(id.clone());
                        return Ok(id);
                    }
                }
            }
            // Check for pagination.
            let next_cursor = json
                .pointer("/response_metadata/next_cursor")
                .and_then(|c| c.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            match next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Err(BamlRtError::InvalidArgument(format!(
            "Slack channel '{target}' not found"
        )))
    }

    /// Fetch messages newer than `oldest` from the channel, paginating up to
    /// `MAX_PAGES` pages to avoid unbounded API calls.
    async fn fetch_messages(
        &self,
        token: &str,
        channel_id: &str,
        oldest: Option<&str>,
    ) -> Result<Vec<serde_json::Value>> {
        let mut all_messages = Vec::new();
        let mut cursor: Option<String> = None;

        let oldest_param = match oldest {
            Some(ts) => ts.to_string(),
            None => {
                // First poll: lookback 24h.
                let lookback = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
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
            if let Some(ref c) = cursor {
                query.push(("cursor", c.clone()));
            }
            let json = self
                .client
                .get_json("conversations.history", token, &query)
                .await
                .map_err(|err| {
                    BamlRtError::ToolExecution(format!("Slack conversations.history failed: {err}"))
                })?;

            if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
                all_messages.extend(messages.iter().cloned());
            }

            let has_more = json
                .get("has_more")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !has_more {
                break;
            }
            let next_cursor = json
                .pointer("/response_metadata/next_cursor")
                .and_then(|c| c.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            match next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        // API returns newest-first; reverse to oldest-first for cursor tracking.
        all_messages.reverse();
        Ok(all_messages)
    }
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

        let channel_id = self.resolve_channel_id(&token, &state).await?;
        state.resolved_channel_id = Some(channel_id.clone());

        let messages = self
            .fetch_messages(&token, &channel_id, state.last_seen_ts.as_deref())
            .await?;

        if messages.is_empty() {
            debug!(
                channel = %self.channel,
                channel_id = %channel_id,
                "no new Slack messages"
            );
            return Ok(ProducerPoll {
                events: vec![],
                checkpoint: state.to_producer_checkpoint(),
            });
        }

        // Update cursor to latest message ts.
        if let Some(latest) = messages.last()
            && let Some(ts) = latest.get("ts").and_then(|t| t.as_str())
        {
            state.last_seen_ts = Some(ts.to_string());
        }

        let source_key = format!("slack:{channel_id}");
        info!(
            channel = %self.channel,
            channel_id = %channel_id,
            message_count = messages.len(),
            "polled new Slack messages"
        );

        let event = ProducedEvent {
            routing_key: AgentDispatchRoutingKey::parse("slack:intake").expect("valid routing key"),
            schema_version: EventSchemaVersion::parse(SCHEMA_VERSION)
                .expect("valid schema version"),
            source_kind: EventSourceKind::parse("slack").expect("valid source kind"),
            source_key: EventSourceKey::parse(&source_key).ok_or_else(|| {
                BamlRtError::ToolExecution(format!(
                    "invalid event source key derived from channel ID: {source_key}"
                ))
            })?,
            messages: vec![json!({
                "channel_id": channel_id,
                "channel": self.channel,
                "messages": messages,
            })],
            context_id: None,
            task_id: None,
            message_id: None,
            metadata: None,
        };

        Ok(ProducerPoll {
            events: vec![event],
            checkpoint: state.to_producer_checkpoint(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_round_trip() {
        let cp = SlackCheckpoint {
            last_seen_ts: Some("1735689600.000001".into()),
            resolved_channel_id: Some("C123ABC".into()),
        };
        let producer_cp = cp.to_producer_checkpoint();
        let restored = SlackCheckpoint::from_producer_checkpoint(&producer_cp);
        assert_eq!(restored.last_seen_ts.as_deref(), Some("1735689600.000001"));
        assert_eq!(restored.resolved_channel_id.as_deref(), Some("C123ABC"));
    }

    #[test]
    fn checkpoint_from_empty_is_default() {
        let cp = SlackCheckpoint::from_producer_checkpoint(&ProducerCheckpoint::none());
        assert!(cp.last_seen_ts.is_none());
        assert!(cp.resolved_channel_id.is_none());
    }

    #[test]
    fn checkpoint_from_garbage_is_default() {
        let cp = SlackCheckpoint::from_producer_checkpoint(&ProducerCheckpoint::some("not json"));
        assert!(cp.last_seen_ts.is_none());
        assert!(cp.resolved_channel_id.is_none());
    }

    #[test]
    fn producer_key_is_namespaced() {
        // Can't call new() without SLACK_BOT_TOKEN, so test the invariant directly.
        let producer = SlackEventProducer {
            client: SlackReadClient::new(),
            channel: "agentium-eng".into(),
            producer_key: "slack:agentium-eng".into(),
            source_kinds: vec![EventSourceKind::parse("slack").unwrap()],
            resolved_channel_id: tokio::sync::OnceCell::new(),
        };
        assert_eq!(producer.producer_key(), "slack:agentium-eng");
        assert_eq!(producer.source_kinds()[0].as_str(), "slack");
    }
}
