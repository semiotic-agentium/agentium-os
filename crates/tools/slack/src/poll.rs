//! Slack channel polling for the task-daemon substrate.
//!
//! Single implementation of `conversations.history` incremental fetch, cursor
//! advancement, and channel resolution. task-daemon adapts [`SlackPollOutcome`]
//! into [`crate::daemon::SourcePoll`] (via its thin `SlackTaskSource` wrapper).

use std::collections::HashSet;

use integrations_slack_read::{SlackAuthPreference, SlackReadClient, SlackReadError};
use reqwest::Url;
use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

use crate::{
    channel::{SlackChannelSelector, resolve_channel_name},
    timestamp::{compact_ts_for_permalink, max_ts, ts_cmp, ts_gt},
};

/// `conversations.history` channel filter (substrate default).
pub const SLACK_HISTORY_CHANNEL_TYPES: &str = "im,mpim,public_channel,private_channel";

/// Per-source cursor and resolution cache persisted by task-daemon or tests.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SlackPollState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_last_seen_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfill_latest_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_channel_id: Option<String>,
}

/// Runtime parameters for one Slack poll cycle.
#[derive(Debug, Clone)]
pub struct SlackPollConfig {
    pub channel: SlackChannelSelector,
    pub history_limit: u16,
    pub max_pages: u16,
    pub auth_preference: SlackAuthPreference,
    pub initial_lookback_seconds: u64,
    pub workspace_url: Option<Url>,
}

impl SlackPollConfig {
    /// Daemon persistence key: `slack:{state_fragment}` (stable before resolution).
    pub fn daemon_state_key(&self) -> String {
        format!("slack:{}", self.channel.state_fragment())
    }

    /// Publish / subscription key after resolution.
    pub fn publish_source_key(channel_id: &str) -> String {
        format!("slack:{channel_id}")
    }
}

/// One message observed in a poll window (API fields + channel context).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackPolledMessage {
    pub channel_id: String,
    pub channel_name: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub text: String,
    pub subtype: Option<String>,
    pub permalink: Option<String>,
}

/// Result of one Slack poll cycle.
#[derive(Debug, Clone)]
pub struct SlackPollOutcome {
    /// `slack:{channel_id}` — wire publish key.
    pub source_key: String,
    /// `#channel` or raw id — operator label.
    pub source_label: String,
    pub channel_id: String,
    /// Messages strictly newer than the committed cursor.
    pub messages: Vec<SlackPolledMessage>,
    /// Count returned from history before `last_seen` filtering.
    pub items_scanned: usize,
    pub state: SlackPollState,
}

#[derive(Debug, Error)]
pub enum SlackPollError {
    #[error("slack poll requires a channel selector")]
    MissingChannel,
    #[error("slack channel '{0}' not found")]
    ChannelNotFound(String),
    #[error("cached slack channel id is stale for selector {selector}")]
    StaleChannelId { selector: String },
    #[error("failed to parse conversations.history response: {0}")]
    HistoryParse(#[from] serde_json::Error),
    #[error(transparent)]
    SlackRead(#[from] SlackReadError),
    #[error(transparent)]
    Resolve(baml_rt_core::BamlRtError),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

struct ResolvedChannel {
    channel_id: String,
    channel_label: String,
}

struct HistoryFetch {
    messages: Vec<SlackPolledMessage>,
    has_more: bool,
}

/// Poll one Slack channel: resolve id, fetch incremental history, advance cursor.
pub async fn poll_slack_channel(
    client: &SlackReadClient,
    config: &SlackPollConfig,
    mut previous: SlackPollState,
) -> Result<SlackPollOutcome, SlackPollError> {
    let (token, _) = client
        .select_token(Some(config.auth_preference), false)
        .map_err(SlackPollError::from)?;

    let mut resolved = resolve_channel(client, token, config, &previous, true).await?;
    previous.resolved_channel_id = Some(resolved.channel_id.clone());

    let now = baml_rt_core::now_unix_secs(baml_rt_core::clock_events::SLACK_SOURCE_BOOTSTRAP);
    let bootstrap_oldest = format!(
        "{}.000000",
        now.saturating_sub(config.initial_lookback_seconds)
    );
    let oldest = previous.last_seen_ts.clone().or(Some(bootstrap_oldest));

    let fetch = match fetch_history(
        client,
        token,
        config,
        &resolved.channel_id,
        &resolved.channel_label,
        oldest.clone(),
        previous.backfill_latest_ts.clone(),
    )
    .await
    {
        Ok(fetch) => fetch,
        Err(error)
            if config.channel.needs_resolution() && is_stale_channel_resolution_error(&error) =>
        {
            warn!(
                channel = %resolved.channel_label,
                stale_channel_id = %resolved.channel_id,
                "cached Slack channel id is stale; re-resolving by channel name"
            );
            previous.resolved_channel_id = None;
            resolved = resolve_channel(client, token, config, &previous, false).await?;
            previous.resolved_channel_id = Some(resolved.channel_id.clone());
            fetch_history(
                client,
                token,
                config,
                &resolved.channel_id,
                &resolved.channel_label,
                oldest,
                previous.backfill_latest_ts.clone(),
            )
            .await?
        }
        Err(error) => return Err(error),
    };

    let raw_latest_ts = fetch.messages.last().map(|message| message.ts.clone());
    let raw_earliest_ts = fetch.messages.first().map(|message| message.ts.clone());

    let items_scanned = fetch.messages.len();
    let mut messages = fetch.messages;
    if let Some(last_seen) = previous.last_seen_ts.as_deref() {
        messages.retain(|message| ts_gt(&message.ts, last_seen));
    }

    let latest_ts = messages.last().map(|message| message.ts.clone());
    let continue_backfill = fetch.has_more && !messages.is_empty();
    if continue_backfill {
        if let Some(raw_earliest_ts) = raw_earliest_ts {
            previous.backfill_latest_ts = Some(raw_earliest_ts);
        }
        previous.pending_last_seen_ts = max_ts(
            previous.pending_last_seen_ts.as_deref(),
            raw_latest_ts.as_deref(),
        );
        warn!(
            channel = %resolved.channel_label,
            channel_id = %resolved.channel_id,
            "Slack history exceeded max_pages; continuing backfill on next poll"
        );
    } else {
        let committed_ts = max_ts(
            previous.pending_last_seen_ts.as_deref(),
            latest_ts.as_deref(),
        );
        previous.pending_last_seen_ts = None;
        previous.backfill_latest_ts = None;
        if let Some(committed_ts) = committed_ts {
            previous.last_seen_ts = Some(committed_ts);
        } else if previous.last_seen_ts.is_none() {
            previous.last_seen_ts = Some(format!("{now}.000000"));
        }
    }

    let source_key = SlackPollConfig::publish_source_key(&resolved.channel_id);
    Ok(SlackPollOutcome {
        source_key,
        source_label: resolved.channel_label.clone(),
        channel_id: resolved.channel_id,
        messages,
        items_scanned,
        state: previous,
    })
}

async fn resolve_channel(
    client: &SlackReadClient,
    token: &str,
    config: &SlackPollConfig,
    previous: &SlackPollState,
    use_cached_resolution: bool,
) -> Result<ResolvedChannel, SlackPollError> {
    if use_cached_resolution && let Some(channel_id) = previous.resolved_channel_id.clone() {
        return Ok(ResolvedChannel {
            channel_id,
            channel_label: config.channel.display_label(),
        });
    }

    let channel_id = match &config.channel {
        SlackChannelSelector::ChannelId(id) => id.clone(),
        SlackChannelSelector::ChannelName(name) => {
            resolve_channel_name(client, token, name).await?
        }
    };

    Ok(ResolvedChannel {
        channel_id,
        channel_label: config.channel.display_label(),
    })
}

async fn fetch_history(
    client: &SlackReadClient,
    token: &str,
    config: &SlackPollConfig,
    channel_id: &str,
    channel_label: &str,
    oldest: Option<String>,
    latest: Option<String>,
) -> Result<HistoryFetch, SlackPollError> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    let mut has_more = false;
    let mut seen_cursors = HashSet::new();

    for _ in 0..config.max_pages {
        let mut query = vec![
            ("channel", channel_id.to_string()),
            ("limit", config.history_limit.to_string()),
            ("types", SLACK_HISTORY_CHANNEL_TYPES.to_string()),
            ("inclusive", "false".to_string()),
        ];
        if let Some(ref oldest) = oldest {
            query.push(("oldest", oldest.clone()));
        }
        if let Some(ref latest) = latest {
            query.push(("latest", latest.clone()));
        }
        if let Some(ref c) = cursor {
            query.push(("cursor", c.clone()));
        }

        let json = client
            .get_json("conversations.history", token, &query)
            .await?;

        let parsed: RawHistoryResponse = serde_json::from_value(json)?;

        for raw in parsed.messages {
            match normalize_message(
                raw,
                channel_id,
                channel_label,
                config.workspace_url.as_ref(),
            ) {
                Some(message) => out.push(message),
                None => warn!(
                    channel_id = %channel_id,
                    "skipping Slack history row without ts"
                ),
            }
        }

        let next_cursor = normalize_cursor(parsed.response_metadata);
        has_more = parsed.has_more && next_cursor.is_some();
        if !has_more {
            break;
        }
        let Some(next) = next_cursor else {
            break;
        };
        if !seen_cursors.insert(next.clone()) {
            return Err(SlackPollError::Internal(anyhow::anyhow!(
                "conversations.history returned a repeated cursor"
            )));
        }
        cursor = Some(next);
    }

    out.sort_by(|left, right| ts_cmp(&left.ts, &right.ts));
    Ok(HistoryFetch {
        messages: out,
        has_more,
    })
}

impl From<baml_rt_core::BamlRtError> for SlackPollError {
    fn from(err: baml_rt_core::BamlRtError) -> Self {
        if let baml_rt_core::BamlRtError::InvalidArgument(ref msg) = err
            && msg.starts_with("Slack channel '")
            && msg.ends_with(" not found")
        {
            return SlackPollError::ChannelNotFound(msg.clone());
        }
        SlackPollError::Resolve(err)
    }
}

fn is_stale_channel_resolution_error(error: &SlackPollError) -> bool {
    matches!(
        error,
        SlackPollError::SlackRead(SlackReadError::ApiError { method, error, .. })
            if *method == "conversations.history"
                && matches!(error.as_str(), "channel_not_found" | "not_in_channel")
    )
}

#[derive(Debug, Deserialize)]
struct RawResponseMetadata {
    #[serde(default)]
    next_cursor: Option<String>,
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
) -> Option<SlackPolledMessage> {
    let ts = raw.ts?;
    let compact_ts = compact_ts_for_permalink(&ts);
    let permalink = raw.permalink.or_else(|| {
        workspace_url.and_then(|workspace| {
            compact_ts.as_ref().and_then(|compact| {
                workspace
                    .join(&format!("archives/{channel_id}/p{compact}"))
                    .ok()
                    .map(|url| url.to_string())
            })
        })
    });

    Some(SlackPolledMessage {
        channel_id: channel_id.to_string(),
        channel_name: channel_name.trim_start_matches('#').to_string(),
        ts,
        thread_ts: raw.thread_ts,
        user_id: raw.user,
        user_name: raw.username,
        text: raw.text.unwrap_or_default(),
        subtype: raw.subtype,
        permalink,
    })
}

/// Raw API-shaped JSON for one Slack history row (daemon + normalize batch).
pub fn slack_history_row_value(
    ts: &str,
    thread_ts: Option<&str>,
    user_id: Option<&str>,
    text: &str,
    subtype: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "ts": ts,
        "thread_ts": thread_ts,
        "user": user_id,
        "text": text,
        "subtype": subtype,
    })
}

/// Raw API-shaped JSON for [`crate::normalize::normalize_polling_batch`].
pub fn polled_messages_to_values(messages: &[SlackPolledMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|message| {
            slack_history_row_value(
                &message.ts,
                message.thread_ts.as_deref(),
                message.user_id.as_deref(),
                &message.text,
                message.subtype.as_deref(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::{Json, Router, extract::Query, routing::get};
    use integrations_slack_read::SlackAuthPreference;
    use serde_json::json;

    use super::*;
    use crate::{
        SlackChannelSelector,
        test_support::http_mock::{ApiHitLog, install_slack_rest_test_env, start_slack_api_mock},
    };

    fn test_poll_config(channel: SlackChannelSelector, max_pages: u16) -> SlackPollConfig {
        SlackPollConfig {
            channel,
            history_limit: 200,
            max_pages,
            auth_preference: SlackAuthPreference::Auto,
            initial_lookback_seconds: 86_400,
            workspace_url: None,
        }
    }

    fn history_message(ts: &str, text: &str) -> serde_json::Value {
        json!({
            "type": "message",
            "user": "U123",
            "text": text,
            "ts": ts,
        })
    }

    fn history_page(
        messages: Vec<serde_json::Value>,
        has_more: bool,
        next_cursor: Option<&str>,
    ) -> serde_json::Value {
        let next_cursor = next_cursor.unwrap_or("");
        json!({
            "ok": true,
            "messages": messages,
            "has_more": has_more,
            "response_metadata": { "next_cursor": next_cursor }
        })
    }

    fn assert_backfill_paused(
        state: &SlackPollState,
        committed_last_seen: &str,
        expected_pending: &str,
        expected_backfill_latest: &str,
    ) {
        assert_eq!(state.last_seen_ts.as_deref(), Some(committed_last_seen));
        assert_eq!(
            state.pending_last_seen_ts.as_deref(),
            Some(expected_pending)
        );
        assert_eq!(
            state.backfill_latest_ts.as_deref(),
            Some(expected_backfill_latest)
        );
    }

    #[tokio::test]
    async fn max_pages_hit_preserves_backfill_state_with_unread_history() {
        let log = ApiHitLog::default();
        let app = Router::new().route(
            "/api/conversations.history",
            get({
                let log = log.clone();
                move |Query(query): Query<HashMap<String, String>>| {
                    let log = log.clone();
                    async move {
                        let channel = query.get("channel").cloned().unwrap_or_default();
                        let cursor = query.get("cursor").cloned();
                        log.push(format!("history channel={channel} cursor={cursor:?}"))
                            .await;

                        let body = if cursor.is_none() {
                            history_page(
                                vec![
                                    history_message("1700000006.000000", "m6"),
                                    history_message("1700000005.000000", "m5"),
                                ],
                                true,
                                Some("cursor-2"),
                            )
                        } else {
                            panic!("unexpected history page: cursor={cursor:?}");
                        };
                        Json(body)
                    }
                }
            }),
        );

        let base_url = start_slack_api_mock(app).await;
        let env = install_slack_rest_test_env(&base_url).await;
        let config = test_poll_config(SlackChannelSelector::ChannelId("C123ABC456".into()), 1);
        let previous = SlackPollState {
            last_seen_ts: Some("1700000000.000000".into()),
            resolved_channel_id: Some("C123ABC456".into()),
            ..Default::default()
        };

        let outcome = poll_slack_channel(&env.client, &config, previous)
            .await
            .expect("poll should succeed");

        assert_eq!(outcome.channel_id, "C123ABC456");
        assert_eq!(outcome.source_key, "slack:C123ABC456");
        assert_eq!(outcome.items_scanned, 2);
        assert_eq!(outcome.messages.len(), 2);
        assert_eq!(outcome.messages[0].ts, "1700000005.000000");
        assert_eq!(outcome.messages[1].ts, "1700000006.000000");
        assert_backfill_paused(
            &outcome.state,
            "1700000000.000000",
            "1700000006.000000",
            "1700000005.000000",
        );

        let hits = log.snapshot().await;
        assert_eq!(hits.len(), 1, "expected one history page, hits={hits:?}");
    }

    #[tokio::test]
    async fn backfill_continuation_commits_cursor_after_bounded_window() {
        let log = ApiHitLog::default();
        let app = Router::new().route(
            "/api/conversations.history",
            get({
                let log = log.clone();
                move |Query(query): Query<HashMap<String, String>>| {
                    let log = log.clone();
                    async move {
                        let channel = query.get("channel").cloned().unwrap_or_default();
                        let latest = query.get("latest").cloned();
                        log.push(format!("history channel={channel} latest={latest:?}"))
                            .await;

                        let body = match latest.as_deref() {
                            None => history_page(
                                vec![
                                    history_message("1700000006.000000", "m6"),
                                    history_message("1700000005.000000", "m5"),
                                ],
                                true,
                                Some("cursor-2"),
                            ),
                            Some("1700000005.000000") => history_page(
                                vec![
                                    history_message("1699999999.000000", "older-2"),
                                    history_message("1699999998.000000", "older-1"),
                                ],
                                false,
                                None,
                            ),
                            other => panic!("unexpected history request latest={other:?}"),
                        };
                        Json(body)
                    }
                }
            }),
        );

        let base_url = start_slack_api_mock(app).await;
        let env = install_slack_rest_test_env(&base_url).await;
        let config = test_poll_config(SlackChannelSelector::ChannelId("C123ABC456".into()), 1);
        let previous = SlackPollState {
            last_seen_ts: Some("1700000000.000000".into()),
            pending_last_seen_ts: Some("1700000006.000000".into()),
            backfill_latest_ts: Some("1700000005.000000".into()),
            resolved_channel_id: Some("C123ABC456".into()),
        };

        let outcome = poll_slack_channel(&env.client, &config, previous)
            .await
            .expect("continuation poll should succeed");

        assert!(
            outcome.messages.is_empty(),
            "backfill window is older than last_seen; only cursor should advance: {:?}",
            outcome.messages
        );
        assert_eq!(
            outcome.state.last_seen_ts.as_deref(),
            Some("1700000006.000000"),
            "pending cursor should commit after bounded backfill completes"
        );
        assert_eq!(outcome.state.pending_last_seen_ts, None);
        assert_eq!(outcome.state.backfill_latest_ts, None);

        let hits = log.snapshot().await;
        assert!(
            hits.iter()
                .any(|hit| hit.contains("latest=Some(\"1700000005.000000\")")),
            "expected continuation poll to bound history with backfill_latest_ts, hits={hits:?}"
        );
    }

    #[tokio::test]
    async fn stale_cached_channel_id_re_resolves_by_name() {
        let log = ApiHitLog::default();
        let app = Router::new()
            .route(
                "/api/conversations.list",
                get({
                    let log = log.clone();
                    move || {
                        let log = log.clone();
                        async move {
                            log.push("list".to_string()).await;
                            Json(json!({
                                "ok": true,
                                "channels": [{ "id": "CNEW12345", "name": "ops" }],
                                "response_metadata": { "next_cursor": "" }
                            }))
                        }
                    }
                }),
            )
            .route(
                "/api/conversations.history",
                get({
                    let log = log.clone();
                    move |Query(query): Query<HashMap<String, String>>| {
                        let log = log.clone();
                        async move {
                            let channel = query.get("channel").cloned().unwrap_or_default();
                            log.push(format!("history channel={channel}")).await;
                            let body = if channel == "COLD12345" {
                                json!({ "ok": false, "error": "channel_not_found" })
                            } else if channel == "CNEW12345" {
                                history_page(
                                    vec![history_message("1700000001.000000", "fresh")],
                                    false,
                                    None,
                                )
                            } else {
                                panic!("unexpected channel requested: {channel}");
                            };
                            Json(body)
                        }
                    }
                }),
            );

        let base_url = start_slack_api_mock(app).await;
        let env = install_slack_rest_test_env(&base_url).await;
        let config = test_poll_config(SlackChannelSelector::ChannelName("ops".into()), 3);
        let previous = SlackPollState {
            resolved_channel_id: Some("COLD12345".into()),
            ..Default::default()
        };

        let outcome = poll_slack_channel(&env.client, &config, previous)
            .await
            .expect("poll should recover from stale cached channel id");

        assert_eq!(outcome.channel_id, "CNEW12345");
        assert_eq!(outcome.source_key, "slack:CNEW12345");
        assert_eq!(
            outcome.state.resolved_channel_id.as_deref(),
            Some("CNEW12345")
        );
        assert_eq!(outcome.messages.len(), 1);
        assert_eq!(outcome.messages[0].text, "fresh");

        let hits = log.snapshot().await;
        assert_eq!(
            hits,
            vec![
                "history channel=COLD12345".to_string(),
                "list".to_string(),
                "history channel=CNEW12345".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn stale_cached_channel_id_does_not_re_resolve_for_channel_id_selector() {
        let log = ApiHitLog::default();
        let app = Router::new()
            .route(
                "/api/conversations.list",
                get({
                    let log = log.clone();
                    move || {
                        let log = log.clone();
                        async move {
                            log.push("list".to_string()).await;
                            Json(json!({ "ok": false, "error": "should-not-call-list" }))
                        }
                    }
                }),
            )
            .route(
                "/api/conversations.history",
                get({
                    let log = log.clone();
                    move |Query(query): Query<HashMap<String, String>>| {
                        let log = log.clone();
                        async move {
                            let channel = query.get("channel").cloned().unwrap_or_default();
                            log.push(format!("history channel={channel}")).await;
                            Json(json!({ "ok": false, "error": "channel_not_found" }))
                        }
                    }
                }),
            );

        let base_url = start_slack_api_mock(app).await;
        let env = install_slack_rest_test_env(&base_url).await;
        let config = test_poll_config(SlackChannelSelector::ChannelId("COLD12345".into()), 3);
        let previous = SlackPollState {
            resolved_channel_id: Some("COLD12345".into()),
            ..Default::default()
        };

        let err = poll_slack_channel(&env.client, &config, previous)
            .await
            .expect_err("channel id selector should not re-resolve by name");

        assert!(
            matches!(
                err,
                SlackPollError::SlackRead(SlackReadError::ApiError { .. })
            ),
            "expected slack api error, got {err:?}"
        );
        let hits = log.snapshot().await;
        assert_eq!(hits, vec!["history channel=COLD12345".to_string()]);
    }
}
