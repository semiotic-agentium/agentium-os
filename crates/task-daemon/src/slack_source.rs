// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Slack polling implementation for [`crate::daemon::TaskSource`].

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use baml_rt_core::clock_events;
use integrations_slack_read::{SlackAuthPreference, SlackReadClient, SlackReadError};
use reqwest::Url;
use serde::Deserialize;

use crate::{
    daemon::{SourcePoll, TaskSource},
    model::{SlackMessage, SourceReference},
    state::TaskDaemonState,
};

const DEFAULT_MAX_PAGES: u16 = 3;

#[derive(Debug, Clone)]
/// Slack channel selector accepted by CLI/config.
pub enum SlackChannelSelector {
    /// Explicit Slack channel id (`C...`, `G...`, `D...`).
    ChannelId(String),
    /// Human channel name (with or without leading `#`).
    ChannelName(String),
}

impl SlackChannelSelector {
    /// Parses a channel selector from CLI/config input.
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim().trim_start_matches('#');
        if trimmed.is_empty() {
            return Err(anyhow!("channel selector must not be empty"));
        }

        let looks_like_id = trimmed.chars().enumerate().all(|(idx, ch)| match idx {
            0 => matches!(ch, 'C' | 'G' | 'D'),
            _ => ch.is_ascii_uppercase() || ch.is_ascii_digit(),
        }) && trimmed.len() >= 9;

        if looks_like_id {
            Ok(Self::ChannelId(trimmed.to_string()))
        } else {
            Ok(Self::ChannelName(trimmed.to_ascii_lowercase()))
        }
    }

    /// Stable fragment used to key persisted source state.
    pub fn state_fragment(&self) -> String {
        match self {
            SlackChannelSelector::ChannelId(id) => id.to_ascii_uppercase(),
            SlackChannelSelector::ChannelName(name) => name.to_ascii_lowercase(),
        }
    }

    fn display_label(&self) -> String {
        match self {
            SlackChannelSelector::ChannelId(id) => id.clone(),
            SlackChannelSelector::ChannelName(name) => format!("#{name}"),
        }
    }
}

#[derive(Debug, Clone)]
/// Runtime configuration for Slack polling.
pub struct SlackSourceConfig {
    pub channel: SlackChannelSelector,
    pub history_limit: u16,
    pub max_pages: u16,
    pub auth_preference: SlackAuthPreference,
    pub initial_lookback_seconds: u64,
    pub workspace_url: Option<Url>,
}

impl Default for SlackSourceConfig {
    fn default() -> Self {
        Self {
            channel: SlackChannelSelector::ChannelName("agentium-eng".to_string()),
            history_limit: 200,
            max_pages: DEFAULT_MAX_PAGES,
            auth_preference: SlackAuthPreference::Auto,
            initial_lookback_seconds: 60 * 60 * 24,
            workspace_url: None,
        }
    }
}

#[derive(Clone)]
/// Slack-backed task source that emits newly observed messages.
pub struct SlackTaskSource {
    client: SlackReadClient,
    config: SlackSourceConfig,
}

struct HistoryFetch {
    messages: Vec<SlackMessage>,
    has_more: bool,
}

impl SlackTaskSource {
    /// Creates a Slack source with the given configuration.
    pub fn new(config: SlackSourceConfig) -> Self {
        Self {
            client: SlackReadClient::new(),
            config,
        }
    }

    async fn resolve_channel(
        &self,
        token: &str,
        source_key: &str,
        state: &mut TaskDaemonState,
        use_cached_resolution: bool,
    ) -> Result<(String, String)> {
        let cached = state.source_state(source_key).cloned().unwrap_or_default();

        if use_cached_resolution && let Some(channel_id) = cached.resolved_id {
            let label = cached
                .resolved_label
                .unwrap_or_else(|| self.config.channel.display_label());
            return Ok((channel_id, label));
        }

        match &self.config.channel {
            SlackChannelSelector::ChannelId(channel_id) => {
                let label = self.config.channel.display_label();
                let source_state = state.source_state_mut(source_key);
                source_state.resolved_id = Some(channel_id.clone());
                source_state.resolved_label = Some(label.clone());
                Ok((channel_id.clone(), label))
            }
            SlackChannelSelector::ChannelName(target_name) => {
                let mut cursor: Option<String> = None;
                let max_pages = self.config.max_pages.max(1);

                for _ in 0..max_pages {
                    let mut query = vec![
                        (
                            "types",
                            "public_channel,private_channel,im,mpim".to_string(),
                        ),
                        ("exclude_archived", "true".to_string()),
                        ("limit", "200".to_string()),
                    ];
                    if let Some(cursor_value) = cursor.clone() {
                        query.push(("cursor", cursor_value));
                    }

                    let json = self
                        .client
                        .get_json("conversations.list", token, &query)
                        .await
                        .context("calling Slack conversations.list")?;
                    let parsed: RawConversationsListResponse = serde_json::from_value(json)
                        .context("parsing conversations.list response")?;

                    if let Some(found) = parsed.channels.into_iter().find(|channel| {
                        channel
                            .name
                            .as_deref()
                            .map(|name| name.eq_ignore_ascii_case(target_name))
                            .unwrap_or(false)
                    }) {
                        let label = found
                            .name
                            .clone()
                            .map(|name| format!("#{name}"))
                            .unwrap_or_else(|| format!("#{target_name}"));
                        let source_state = state.source_state_mut(source_key);
                        source_state.resolved_id = Some(found.id.clone());
                        source_state.resolved_label = Some(label.clone());
                        return Ok((found.id, label));
                    }

                    cursor = normalize_cursor(parsed.response_metadata);
                    if cursor.is_none() {
                        break;
                    }
                }

                Err(anyhow!(
                    "could not resolve Slack channel named #{target_name}; verify the token can access that channel"
                ))
            }
        }
    }

    async fn fetch_history(
        &self,
        token: &str,
        channel_id: &str,
        oldest: Option<String>,
        latest: Option<String>,
        channel_name: &str,
    ) -> Result<HistoryFetch> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        let max_pages = self.config.max_pages.max(1);
        let mut has_more = false;

        for _ in 0..max_pages {
            let mut query = vec![
                ("channel", channel_id.to_string()),
                (
                    "limit",
                    self.config.history_limit.clamp(1, 1_000).to_string(),
                ),
            ];
            let mut has_bound = false;
            if let Some(ref oldest_ts) = oldest {
                query.push(("oldest", oldest_ts.clone()));
                has_bound = true;
            }
            if let Some(ref latest_ts) = latest {
                query.push(("latest", latest_ts.clone()));
                has_bound = true;
            }
            if has_bound {
                query.push(("inclusive", "false".to_string()));
            }
            if let Some(cursor_value) = cursor.clone() {
                query.push(("cursor", cursor_value));
            }

            let json = self
                .client
                .get_json("conversations.history", token, &query)
                .await
                .context("calling Slack conversations.history")?;
            let parsed: RawHistoryResponse =
                serde_json::from_value(json).context("parsing conversations.history response")?;

            out.extend(parsed.messages.into_iter().filter_map(|raw| {
                normalize_message(
                    raw,
                    channel_id,
                    channel_name,
                    self.config.workspace_url.as_ref(),
                )
            }));

            let next_cursor = normalize_cursor(parsed.response_metadata);
            has_more = parsed.has_more && next_cursor.is_some();
            if !has_more {
                break;
            }
            cursor = next_cursor;
        }

        out.sort_by(|left, right| ts_cmp(&left.ts, &right.ts));
        Ok(HistoryFetch {
            messages: out,
            has_more,
        })
    }
}

#[async_trait]
impl TaskSource for SlackTaskSource {
    fn source_key(&self) -> String {
        format!("slack:{}", self.config.channel.state_fragment())
    }

    async fn poll(&mut self, state: &mut TaskDaemonState) -> Result<SourcePoll> {
        let source_key = self.source_key();
        let (token, _) = self
            .client
            .select_token(Some(self.config.auth_preference), false)
            .context("selecting Slack token for task daemon")?;

        let last_seen_ts = state
            .source_state(&source_key)
            .and_then(|source_state| source_state.last_seen_ts.clone());
        let backfill_latest_ts = state
            .source_state(&source_key)
            .and_then(|source_state| source_state.backfill_latest_ts.clone());
        let (mut channel_id, mut channel_label) = self
            .resolve_channel(token, &source_key, state, true)
            .await?;

        let now = baml_rt_core::now_unix_secs(clock_events::SLACK_SOURCE_BOOTSTRAP);
        let bootstrap_oldest = format!(
            "{}.000000",
            now.saturating_sub(self.config.initial_lookback_seconds)
        );
        let oldest = last_seen_ts
            .clone()
            .or_else(|| Some(bootstrap_oldest.clone()));

        let fetch = match self
            .fetch_history(
                token,
                &channel_id,
                oldest.clone(),
                backfill_latest_ts.clone(),
                &channel_label,
            )
            .await
        {
            Ok(fetch) => fetch,
            Err(error)
                if matches!(self.config.channel, SlackChannelSelector::ChannelName(_))
                    && is_stale_channel_resolution_error(&error) =>
            {
                tracing::warn!(
                    source_key = %source_key,
                    stale_channel_id = %channel_id,
                    "cached Slack channel id is stale; re-resolving by channel name"
                );
                {
                    let source_state = state.source_state_mut(&source_key);
                    source_state.resolved_id = None;
                    source_state.resolved_label = None;
                }
                let refreshed = self
                    .resolve_channel(token, &source_key, state, false)
                    .await?;
                channel_id = refreshed.0;
                channel_label = refreshed.1;
                self.fetch_history(
                    token,
                    &channel_id,
                    oldest.clone(),
                    backfill_latest_ts.clone(),
                    &channel_label,
                )
                .await?
            }
            Err(error) => return Err(error),
        };

        let raw_latest_ts = fetch.messages.last().map(|message| message.ts.clone());
        let raw_earliest_ts = fetch.messages.first().map(|message| message.ts.clone());

        let mut messages = fetch.messages;
        if let Some(last_seen) = last_seen_ts.as_deref() {
            messages.retain(|message| ts_gt(&message.ts, last_seen));
        }

        let latest_ts = messages.last().map(|message| message.ts.clone());
        let source_state = state.source_state_mut(&source_key);
        source_state.resolved_id = Some(channel_id.clone());
        source_state.resolved_label = Some(channel_label.clone());

        let continue_backfill = fetch.has_more && !messages.is_empty();
        if continue_backfill {
            if let Some(raw_earliest_ts) = raw_earliest_ts {
                source_state.backfill_latest_ts = Some(raw_earliest_ts);
            }
            source_state.pending_last_seen_ts = max_ts(
                source_state.pending_last_seen_ts.as_deref(),
                raw_latest_ts.as_deref(),
            );
            tracing::warn!(
                source_key = %source_key,
                channel = %channel_label,
                "Slack history exceeded max_pages; continuing backfill on next poll"
            );
        } else {
            let committed_ts = max_ts(
                source_state.pending_last_seen_ts.as_deref(),
                latest_ts.as_deref(),
            );
            source_state.pending_last_seen_ts = None;
            source_state.backfill_latest_ts = None;
            if let Some(committed_ts) = committed_ts {
                source_state.last_seen_ts = Some(committed_ts);
            } else if source_state.last_seen_ts.is_none() {
                // Bootstrap a cursor when no messages are returned so polling stays incremental.
                source_state.last_seen_ts = Some(format!("{now}.000000"));
            }
        }

        let source_items_scanned = messages.len();

        Ok(SourcePoll::slack(
            source_key,
            channel_label,
            messages,
            source_items_scanned,
        ))
    }
}

fn is_stale_channel_resolution_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<SlackReadError>()
            .is_some_and(|slack_error| {
                matches!(
                    slack_error,
                    SlackReadError::ApiError { method, error, .. }
                        if *method == "conversations.history"
                            && matches!(error.as_str(), "channel_not_found" | "not_in_channel")
                )
            })
    })
}

fn max_ts(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => {
            if ts_gt(left, right) {
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

#[derive(Debug, Deserialize)]
struct RawResponseMetadata {
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawConversation {
    id: String,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawConversationsListResponse {
    #[serde(default)]
    channels: Vec<RawConversation>,
    response_metadata: Option<RawResponseMetadata>,
}

#[derive(Debug, Deserialize)]
struct RawHistoryResponse {
    #[serde(default)]
    messages: Vec<RawMessage>,
    #[serde(default)]
    has_more: bool,
    response_metadata: Option<RawResponseMetadata>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    ts: Option<String>,
    thread_ts: Option<String>,
    user: Option<String>,
    text: Option<String>,
    subtype: Option<String>,
    username: Option<String>,
    permalink: Option<String>,
}

fn normalize_cursor(metadata: Option<RawResponseMetadata>) -> Option<String> {
    metadata
        .and_then(|meta| meta.next_cursor)
        .map(|cursor| cursor.trim().to_string())
        .filter(|cursor| !cursor.is_empty())
}

fn normalize_message(
    raw: RawMessage,
    channel_id: &str,
    channel_name: &str,
    workspace_url: Option<&Url>,
) -> Option<SlackMessage> {
    let ts = raw.ts?;
    let compact_ts = compact_ts(&ts);

    let source = SourceReference {
        reference: match compact_ts.as_deref() {
            Some(compact) => format!("slack://channel/{channel_id}/p{compact}"),
            None => format!("slack://channel/{channel_id}/ts/{ts}"),
        },
        permalink: raw.permalink.or_else(|| {
            workspace_url.and_then(|workspace| {
                compact_ts.as_ref().and_then(|compact| {
                    workspace
                        .join(&format!("archives/{channel_id}/p{compact}"))
                        .ok()
                        .map(|url| url.to_string())
                })
            })
        }),
        channel_id: Some(channel_id.to_string()),
        message_ts: Some(ts.clone()),
        thread_ts: raw.thread_ts.clone(),
    };

    Some(SlackMessage {
        channel_name: channel_name.trim_start_matches('#').to_string(),
        channel_id: channel_id.to_string(),
        ts,
        thread_ts: raw.thread_ts,
        user_id: raw.user,
        user_name: raw.username,
        text: raw.text.unwrap_or_default(),
        subtype: raw.subtype,
        source,
    })
}

fn compact_ts(ts: &str) -> Option<String> {
    let (left, right) = ts.split_once('.')?;
    if left.len() < 9 || left.len() > 10 {
        return None;
    }
    let mut micros = right.chars().take(6).collect::<String>();
    while micros.len() < 6 {
        micros.push('0');
    }
    Some(format!("{left}{micros}"))
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

fn ts_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_slack_timestamp(left), parse_slack_timestamp(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

#[cfg(test)]
mod tests {
    use integrations_slack_read::SlackApiErrorClass;

    use super::*;

    #[test]
    fn parses_channel_selector() {
        let by_name = SlackChannelSelector::parse("#agentium-eng").expect("selector by name");
        assert!(matches!(
            by_name,
            SlackChannelSelector::ChannelName(ref name) if name == "agentium-eng"
        ));

        let by_id = SlackChannelSelector::parse("C123ABCDEF").expect("selector by id");
        assert!(matches!(
            by_id,
            SlackChannelSelector::ChannelId(ref id) if id == "C123ABCDEF"
        ));
    }

    #[test]
    fn compacts_ts() {
        assert_eq!(
            compact_ts("1735689600.1").as_deref(),
            Some("1735689600100000")
        );
        assert_eq!(
            compact_ts("1735689600.123456").as_deref(),
            Some("1735689600123456")
        );
    }

    #[test]
    fn detects_stale_channel_id_error_through_context_chain() {
        let error = anyhow::Error::new(SlackReadError::ApiError {
            method: "conversations.history",
            error: "channel_not_found".to_string(),
            class: SlackApiErrorClass::InvalidArgument,
        })
        .context("calling Slack conversations.history");
        assert!(is_stale_channel_resolution_error(&error));
    }

    #[test]
    fn max_ts_prefers_newer_timestamp() {
        assert_eq!(
            max_ts(Some("1735689600.000000"), Some("1735689700.000000")).as_deref(),
            Some("1735689700.000000")
        );
        assert_eq!(
            max_ts(Some("1735689700.000000"), Some("1735689600.000000")).as_deref(),
            Some("1735689700.000000")
        );
    }

    #[test]
    fn compares_timestamps_at_microsecond_precision() {
        assert_eq!(
            ts_cmp("1735689600.123456", "1735689600.123457"),
            std::cmp::Ordering::Less
        );
        assert!(ts_gt("1735689600.123457", "1735689600.123456"));
    }
}
