//! Slack channel selectors and name→id resolution shared by task-daemon and runner ingress.

use std::collections::HashSet;

use baml_rt_core::{BamlRtError, Result, event_subscription::EventSourceKey};
use integrations_slack_read::{SlackReadClient, SlackReadError};

use crate::SlackError;

/// Channel selector from config/CLI (`#name` or `C…` id).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SlackChannelSelector {
    /// Explicit Slack channel id (`C…`, `G…`, `D…`).
    ChannelId(String),
    /// Human channel name (without leading `#`).
    ChannelName(String),
}

impl SlackChannelSelector {
    /// Parses a channel selector from config or CLI input.
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim().trim_start_matches('#');
        if trimmed.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "slack channel selector must not be empty".to_string(),
            ));
        }

        if looks_like_channel_id(trimmed) {
            Ok(Self::ChannelId(trimmed.to_ascii_uppercase()))
        } else {
            Ok(Self::ChannelName(trimmed.to_ascii_lowercase()))
        }
    }

    /// Stable fragment for persisted daemon state keys (`slack:{fragment}`).
    pub fn state_fragment(&self) -> String {
        match self {
            Self::ChannelId(id) => id.to_ascii_uppercase(),
            Self::ChannelName(name) => name.to_ascii_lowercase(),
        }
    }

    /// Human-facing label (`#name` or raw id).
    pub fn display_label(&self) -> String {
        match self {
            Self::ChannelId(id) => id.clone(),
            Self::ChannelName(name) => format!("#{name}"),
        }
    }

    /// Runner producer-key suffix (`name:ops` / `id:C123…`).
    pub fn producer_key_fragment(&self) -> String {
        match self {
            Self::ChannelId(id) => format!("id:{id}"),
            Self::ChannelName(name) => format!("name:{name}"),
        }
    }

    /// Whether this selector requires `conversations.list` resolution.
    pub fn needs_resolution(&self) -> bool {
        matches!(self, Self::ChannelName(_))
    }
}

/// Returns true when `raw` looks like a Slack channel id (not a name like `CLUBROOMS`).
pub fn looks_like_channel_id(raw: &str) -> bool {
    raw.len() >= 9
        && raw.chars().skip(1).any(|ch| ch.is_ascii_digit())
        && raw.chars().enumerate().all(|(idx, ch)| match idx {
            0 => matches!(ch, 'C' | 'D' | 'G'),
            _ => ch.is_ascii_uppercase() || ch.is_ascii_digit(),
        })
}

/// Canonical `EventSourceKey` for Slack REST polling after channel id is known.
pub fn slack_polling_source_key(channel_id: &str) -> Result<EventSourceKey> {
    EventSourceKey::parse(format!("slack:{channel_id}")).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "invalid Slack polling source key for channel id: slack:{channel_id}"
        ))
    })
}

/// Resolve a selector to a channel id (no-op for [`SlackChannelSelector::ChannelId`]).
pub async fn resolve_selector_channel_id(
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

/// Resolve a channel name via paginated `conversations.list`.
pub async fn resolve_channel_name(
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

fn map_slack_read_error(error: SlackReadError) -> BamlRtError {
    BamlRtError::from(SlackError::from(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_channel_id_accepts_slack_ids() {
        assert!(looks_like_channel_id("C123ABC456"));
        assert!(looks_like_channel_id("G12345678"));
        assert!(!looks_like_channel_id("clubrooms"));
        assert!(!looks_like_channel_id("C12"));
    }

    #[test]
    fn selector_parse_normalizes_case() {
        let by_id = SlackChannelSelector::parse("C123ABC456").expect("channel id");
        assert!(matches!(by_id, SlackChannelSelector::ChannelId(id) if id == "C123ABC456"));

        let by_name = SlackChannelSelector::parse("#Ops").expect("channel name");
        assert!(matches!(by_name, SlackChannelSelector::ChannelName(name) if name == "ops"));
    }
}
