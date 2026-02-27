//! Slack tool — `support/slack`.
//!
//! Read-only Slack Web API integration for conversation and thread retrieval.
//! Supports:
//! - listing conversations
//! - conversation history retrieval
//! - thread replies retrieval
//! - user resolution by ID
//! - message search (user-token scope)

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_derive_core::BamlType as BamlTypeTrait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolMetadataBuilder, TypeBasedMetadataBuilder,
    bundles::Support,
    parse_tool_name_and_class, register_tool,
    tools::{
        BamlTool, ToolAccess, ToolFunctionMetadata, ToolHandler, ToolSecretRequirement,
        create_tool_handler,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Slack API base URL.
pub const BASE_URL: &str = "https://slack.com/api";

const MAX_RATE_LIMIT_RETRIES: usize = 3;
const RATE_LIMIT_BASE_DELAY_MS: u64 = 500;
const RATE_LIMIT_MAX_DELAY_MS: u64 = 5_000;

fn backoff_delay(retries: usize) -> Duration {
    let shift = u32::try_from(retries).unwrap_or(u32::MAX);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let backoff = RATE_LIMIT_BASE_DELAY_MS.saturating_mul(multiplier);
    Duration::from_millis(backoff.min(RATE_LIMIT_MAX_DELAY_MS))
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS, BamlType,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SlackAuthPreference {
    #[default]
    Auto,
    Bot,
    User,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SlackConversationKind {
    PublicChannel,
    PrivateChannel,
    Im,
    Mpim,
}

impl SlackConversationKind {
    fn as_api_type(self) -> &'static str {
        match self {
            SlackConversationKind::PublicChannel => "public_channel",
            SlackConversationKind::PrivateChannel => "private_channel",
            SlackConversationKind::Im => "im",
            SlackConversationKind::Mpim => "mpim",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SlackHistoryOrder {
    #[default]
    LatestFirst,
    OldestFirst,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SlackUserResolutionMode {
    None,
    #[default]
    ResolveUsers,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SlackSearchSort {
    #[default]
    Score,
    Timestamp,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SlackSearchDirection {
    Asc,
    #[default]
    Desc,
}

/// List conversations available to the authorized token.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ListConversationsInput {
    /// Slack conversation kinds to include.
    pub kinds: Vec<SlackConversationKind>,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
    pub exclude_archived: Option<bool>,
    pub include_num_members: Option<bool>,
    pub auth: Option<SlackAuthPreference>,
}

/// Retrieve messages from a channel conversation history.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct GetConversationHistoryInput {
    pub channel_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
    pub oldest: Option<String>,
    pub latest: Option<String>,
    pub inclusive: Option<bool>,
    pub order: Option<SlackHistoryOrder>,
    pub resolve_users: Option<SlackUserResolutionMode>,
    pub auth: Option<SlackAuthPreference>,
}

/// Retrieve replies in a thread.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct GetThreadRepliesInput {
    pub channel_id: String,
    pub thread_ts: String,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
    pub oldest: Option<String>,
    pub latest: Option<String>,
    pub inclusive: Option<bool>,
    pub order: Option<SlackHistoryOrder>,
    pub resolve_users: Option<SlackUserResolutionMode>,
    pub auth: Option<SlackAuthPreference>,
}

/// Resolve specific Slack user IDs to display data.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ResolveUsersInput {
    pub user_ids: Vec<String>,
    pub auth: Option<SlackAuthPreference>,
}

/// Search Slack messages.
///
/// Requires a user token (`SLACK_USER_TOKEN`) in most Slack orgs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SearchMessagesInput {
    pub query: String,
    pub count: Option<u16>,
    pub page: Option<u16>,
    pub sort: Option<SlackSearchSort>,
    pub direction: Option<SlackSearchDirection>,
    pub resolve_users: Option<SlackUserResolutionMode>,
    pub auth: Option<SlackAuthPreference>,
}

/// Typed action union for `support/slack`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[baml(union)]
#[serde(untagged)]
#[ts(export)]
pub enum SlackInput {
    GetThreadReplies(GetThreadRepliesInput),
    GetConversationHistory(GetConversationHistoryInput),
    SearchMessages(SearchMessagesInput),
    ResolveUsers(ResolveUsersInput),
    ListConversations(ListConversationsInput),
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SlackSourceKind {
    Conversation,
    Message,
    ThreadReply,
    SearchMatch,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct SlackSource {
    pub kind: SlackSourceKind,
    /// Stable Slack reference (for example `slack://channel/C123/p1700000000000000`).
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct SlackConversationSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: SlackConversationKind,
    pub is_archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_member: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_members: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct SlackUserSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub real_name: Option<String>,
    pub is_bot: bool,
    pub is_deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct SlackMessageSummary {
    pub channel_id: String,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    pub source_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct SlackOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversations: Vec<SlackConversationSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<SlackMessageSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<SlackUserSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SlackSource>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum SlackApiErrorClass {
    Configuration,
    InvalidArgument,
    ToolExecution,
}

#[derive(Debug, thiserror::Error)]
pub enum SlackError {
    #[error("Slack HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("Slack API authentication failed ({status}): {body}")]
    Unauthorized {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Slack rate limit exceeded (429), retry after {retry_after}: {body}")]
    RateLimited {
        body: String,
        retry_after: RetryAfter,
    },

    #[error("Slack API returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Slack API {method} failed: {error}")]
    ApiError {
        method: &'static str,
        error: String,
        class: SlackApiErrorClass,
    },

    #[error("Failed to deserialize Slack response")]
    Deserialize(#[source] reqwest::Error),

    #[error("Unexpected Slack response shape: {message}")]
    UnexpectedShape { message: String },

    #[error("{message}")]
    MissingToken { message: String },

    #[error("Invalid argument: {message}")]
    InvalidArgument { message: String },

    #[error("Invalid Slack header value: {message}")]
    InvalidHeader { message: String },

    #[error("Failed to clone Slack request")]
    RequestClone,
}

impl From<SlackError> for BamlRtError {
    fn from(err: SlackError) -> Self {
        match &err {
            SlackError::MissingToken { .. }
            | SlackError::Unauthorized { .. }
            | SlackError::InvalidHeader { .. }
            | SlackError::ApiError {
                class: SlackApiErrorClass::Configuration,
                ..
            } => BamlRtError::Configuration(err.to_string()),
            SlackError::InvalidArgument { .. }
            | SlackError::ApiError {
                class: SlackApiErrorClass::InvalidArgument,
                ..
            } => BamlRtError::InvalidArgument(err.to_string()),
            SlackError::Http(_)
            | SlackError::RateLimited { .. }
            | SlackError::Api { .. }
            | SlackError::Deserialize(_)
            | SlackError::UnexpectedShape { .. }
            | SlackError::ApiError {
                class: SlackApiErrorClass::ToolExecution,
                ..
            }
            | SlackError::RequestClone => BamlRtError::ToolExecution(err.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RetryAfter {
    Seconds(u64),
    Unknown(String),
    Missing,
}

impl RetryAfter {
    fn as_duration(&self) -> Option<Duration> {
        match self {
            RetryAfter::Seconds(seconds) => Some(Duration::from_secs(*seconds)),
            RetryAfter::Unknown(_) | RetryAfter::Missing => None,
        }
    }
}

impl std::fmt::Display for RetryAfter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetryAfter::Seconds(seconds) => write!(f, "{seconds}s"),
            RetryAfter::Unknown(raw) => write!(f, "unknown({raw})"),
            RetryAfter::Missing => write!(f, "missing"),
        }
    }
}

// ---------------------------------------------------------------------------
// Raw response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawResponseMetadata {
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawConversation {
    id: String,
    name: Option<String>,
    #[serde(default)]
    is_group: bool,
    #[serde(default)]
    is_private: bool,
    #[serde(default)]
    is_im: bool,
    #[serde(default)]
    is_mpim: bool,
    #[serde(default)]
    is_archived: bool,
    is_member: Option<bool>,
    num_members: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawConversationsListResponse {
    #[serde(default)]
    channels: Vec<RawConversation>,
    response_metadata: Option<RawResponseMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMessage {
    ts: Option<String>,
    thread_ts: Option<String>,
    user: Option<String>,
    text: Option<String>,
    subtype: Option<String>,
    username: Option<String>,
    permalink: Option<String>,
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
struct RawUserProfile {
    display_name: Option<String>,
    real_name: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawUser {
    id: String,
    name: Option<String>,
    profile: Option<RawUserProfile>,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    is_bot: bool,
}

#[derive(Debug, Deserialize)]
struct RawUserInfoResponse {
    user: RawUser,
}

#[derive(Debug, Deserialize)]
struct RawSearchChannel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct RawSearchMessage {
    channel: RawSearchChannel,
    ts: Option<String>,
    thread_ts: Option<String>,
    user: Option<String>,
    text: Option<String>,
    subtype: Option<String>,
    permalink: Option<String>,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSearchPagination {
    page: Option<u32>,
    page_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawSearchMessagesContainer {
    #[serde(default)]
    matches: Vec<RawSearchMessage>,
    pagination: Option<RawSearchPagination>,
}

#[derive(Debug, Deserialize)]
struct RawSearchMessagesResponse {
    messages: RawSearchMessagesContainer,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum SlackTokenKind {
    Bot,
    User,
}

#[derive(Debug, Clone)]
struct SlackAuthConfig {
    bot_token: Option<String>,
    user_token: Option<String>,
}

impl SlackAuthConfig {
    fn from_env() -> Self {
        Self {
            bot_token: std::env::var("SLACK_BOT_TOKEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            user_token: std::env::var("SLACK_USER_TOKEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        }
    }

    fn select_token(
        &self,
        preference: Option<SlackAuthPreference>,
        requires_user: bool,
    ) -> std::result::Result<(&str, SlackTokenKind), SlackError> {
        let preference = preference.unwrap_or_default();
        if requires_user {
            if preference == SlackAuthPreference::Bot {
                return Err(SlackError::MissingToken {
                    message: "Search requires a user token; auth=bot is not supported".to_string(),
                });
            }
            return self
                .user_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::User))
                .ok_or_else(|| SlackError::MissingToken {
                    message: "SLACK_USER_TOKEN environment variable is required for message search"
                        .to_string(),
                });
        }

        match preference {
            SlackAuthPreference::Bot => self
                .bot_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::Bot))
                .ok_or_else(|| SlackError::MissingToken {
                    message: "SLACK_BOT_TOKEN environment variable is required when auth=bot"
                        .to_string(),
                }),
            SlackAuthPreference::User => self
                .user_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::User))
                .ok_or_else(|| SlackError::MissingToken {
                    message: "SLACK_USER_TOKEN environment variable is required when auth=user"
                        .to_string(),
                }),
            SlackAuthPreference::Auto => self
                .bot_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::Bot))
                .or_else(|| {
                    self.user_token
                        .as_deref()
                        .map(|token| (token, SlackTokenKind::User))
                })
                .ok_or_else(|| SlackError::MissingToken {
                    message: "Set SLACK_BOT_TOKEN (preferred) or SLACK_USER_TOKEN".to_string(),
                }),
        }
    }
}

#[derive(Clone)]
struct SlackClient {
    client: reqwest::Client,
    auth: SlackAuthConfig,
    base_url: String,
}

impl SlackClient {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            auth: SlackAuthConfig::from_env(),
            base_url: Self::resolve_base_url(),
        }
    }

    fn resolve_base_url() -> String {
        let override_url = std::env::var("SLACK_API_BASE_URL")
            .ok()
            .map(|raw| raw.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty());
        if let Some(base_url) = override_url {
            if !cfg!(test) && should_warn_on_insecure_base_url(&base_url) {
                tracing::warn!(
                    base_url = %base_url,
                    "SLACK_API_BASE_URL is not https; bearer token may be sent to an insecure endpoint"
                );
            }
            return base_url;
        }
        BASE_URL.to_string()
    }

    fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    fn authorized_get(
        &self,
        token: &str,
        method: &'static str,
    ) -> std::result::Result<reqwest::RequestBuilder, SlackError> {
        let endpoint = format!("{}/{}", self.base_url(), method);
        let authorization = format!("Bearer {token}")
            .parse::<reqwest::header::HeaderValue>()
            .map_err(|e| SlackError::InvalidHeader {
                message: e.to_string(),
            })?;
        Ok(self
            .client
            .get(endpoint)
            .header(reqwest::header::AUTHORIZATION, authorization))
    }

    async fn send_request(
        &self,
        method_name: &'static str,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, SlackError> {
        let request = request.build().map_err(SlackError::Http)?;
        let mut retries = 0usize;
        loop {
            let req = request.try_clone().ok_or(SlackError::RequestClone)?;
            let resp = self.client.execute(req).await.map_err(SlackError::Http)?;
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = Self::parse_retry_after(resp.headers().get("retry-after"));
                let body = resp.text().await.unwrap_or_default();
                if retries < MAX_RATE_LIMIT_RETRIES {
                    let delay = retry_after
                        .as_duration()
                        .unwrap_or_else(|| backoff_delay(retries));
                    tracing::warn!(
                        method = method_name,
                        retries = retries + 1,
                        retry_after = %retry_after,
                        "Slack rate limit hit; backing off"
                    );
                    retries += 1;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(SlackError::RateLimited { body, retry_after });
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(match status {
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                        SlackError::Unauthorized { status, body }
                    }
                    _ => SlackError::Api { status, body },
                });
            }

            let json: serde_json::Value = resp.json().await.map_err(SlackError::Deserialize)?;
            let ok = json.get("ok").and_then(serde_json::Value::as_bool);
            match ok {
                Some(true) => return Ok(json),
                Some(false) => {
                    let code = json
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown_error");
                    return Err(map_slack_api_error(method_name, code.to_string()));
                }
                None => {
                    return Err(SlackError::UnexpectedShape {
                        message: format!("missing 'ok' field for {method_name}"),
                    });
                }
            }
        }
    }

    fn parse_retry_after(value: Option<&reqwest::header::HeaderValue>) -> RetryAfter {
        let Some(value) = value else {
            return RetryAfter::Missing;
        };
        let raw = match value.to_str() {
            Ok(raw) => raw,
            Err(_) => return RetryAfter::Unknown("invalid-utf8".to_string()),
        };
        match raw.trim().parse::<u64>() {
            Ok(seconds) => RetryAfter::Seconds(seconds),
            Err(_) => RetryAfter::Unknown(raw.to_string()),
        }
    }

    async fn list_conversations(&self, input: ListConversationsInput) -> Result<SlackOutput> {
        let (token, _token_kind) = self.auth.select_token(input.auth, false)?;
        let kinds_param = conversation_types_param(&input.kinds);
        let limit = input.limit.unwrap_or(100).clamp(1, 1_000);
        let request = self.authorized_get(token, "conversations.list")?.query(&[
            ("types", kinds_param),
            (
                "exclude_archived",
                input.exclude_archived.unwrap_or(true).to_string(),
            ),
            ("limit", limit.to_string()),
        ]);
        let request = if let Some(cursor) = input.cursor.as_deref() {
            request.query(&[("cursor", cursor)])
        } else {
            request
        };
        let request = if let Some(include_num_members) = input.include_num_members {
            request.query(&[("include_num_members", include_num_members)])
        } else {
            request
        };
        let json = self.send_request("conversations.list", request).await?;
        let parsed: RawConversationsListResponse =
            serde_json::from_value(json).map_err(|e| SlackError::UnexpectedShape {
                message: format!("unexpected conversations.list shape: {e}"),
            })?;
        let conversations: Vec<SlackConversationSummary> = parsed
            .channels
            .into_iter()
            .map(|channel| {
                let kind = map_conversation_kind(&channel);
                SlackConversationSummary {
                    id: channel.id,
                    name: channel.name,
                    kind,
                    is_archived: channel.is_archived,
                    is_member: channel.is_member,
                    num_members: channel.num_members,
                }
            })
            .collect();
        let next_cursor = normalize_cursor(parsed.response_metadata.as_ref());
        let has_more = next_cursor.is_some();
        let sources = dedupe_sources(
            conversations
                .iter()
                .map(|conversation| SlackSource {
                    kind: SlackSourceKind::Conversation,
                    reference: format!("slack://channel/{}", conversation.id),
                    permalink: None,
                    channel_id: Some(conversation.id.clone()),
                    message_ts: None,
                    thread_ts: None,
                })
                .collect(),
        );
        let count = conversations.len();
        Ok(SlackOutput {
            conversations,
            messages: Vec::new(),
            users: Vec::new(),
            next_cursor,
            has_more,
            sources,
            message: format!("Retrieved {count} conversation(s)"),
        })
    }

    async fn get_conversation_history(
        &self,
        input: GetConversationHistoryInput,
    ) -> Result<SlackOutput> {
        if input.channel_id.trim().is_empty() {
            return Err(SlackError::InvalidArgument {
                message: "channel_id must not be empty".to_string(),
            }
            .into());
        }
        let (token, _token_kind) = self.auth.select_token(input.auth, false)?;
        let limit = input.limit.unwrap_or(100).clamp(1, 1_000);
        let request = self
            .authorized_get(token, "conversations.history")?
            .query(&[
                ("channel", input.channel_id.as_str()),
                ("limit", &limit.to_string()),
            ]);
        let request = apply_history_range_params(
            request,
            input.cursor.as_deref(),
            input.oldest.as_deref(),
            input.latest.as_deref(),
            input.inclusive,
        );
        let json = self.send_request("conversations.history", request).await?;
        let parsed: RawHistoryResponse =
            serde_json::from_value(json).map_err(|e| SlackError::UnexpectedShape {
                message: format!("unexpected conversations.history shape: {e}"),
            })?;
        let resolve_mode = input.resolve_users.unwrap_or_default();
        let mut messages = normalize_messages(
            parsed.messages,
            &input.channel_id,
            SlackSourceKind::Message,
            input.order.unwrap_or_default(),
        );
        let users = if matches!(resolve_mode, SlackUserResolutionMode::ResolveUsers) {
            self.resolve_users_for_messages(&messages, input.auth).await
        } else {
            Vec::new()
        };
        attach_user_names(&mut messages, &users);
        let next_cursor = normalize_cursor(parsed.response_metadata.as_ref());
        let has_more = parsed.has_more || next_cursor.is_some();
        let sources = dedupe_sources(
            messages
                .iter()
                .map(|message| SlackSource {
                    kind: SlackSourceKind::Message,
                    reference: message.source_ref.clone(),
                    permalink: message.permalink.clone(),
                    channel_id: Some(message.channel_id.clone()),
                    message_ts: Some(message.ts.clone()),
                    thread_ts: message.thread_ts.clone(),
                })
                .collect(),
        );
        let count = messages.len();
        Ok(SlackOutput {
            conversations: Vec::new(),
            messages,
            users,
            next_cursor,
            has_more,
            sources,
            message: format!("Retrieved {count} message(s) from {}", input.channel_id),
        })
    }

    async fn get_thread_replies(&self, input: GetThreadRepliesInput) -> Result<SlackOutput> {
        if input.channel_id.trim().is_empty() {
            return Err(SlackError::InvalidArgument {
                message: "channel_id must not be empty".to_string(),
            }
            .into());
        }
        if input.thread_ts.trim().is_empty() {
            return Err(SlackError::InvalidArgument {
                message: "thread_ts must not be empty".to_string(),
            }
            .into());
        }
        let (token, _token_kind) = self.auth.select_token(input.auth, false)?;
        let limit = input.limit.unwrap_or(100).clamp(1, 1_000);
        let request = self
            .authorized_get(token, "conversations.replies")?
            .query(&[
                ("channel", input.channel_id.as_str()),
                ("ts", input.thread_ts.as_str()),
                ("limit", &limit.to_string()),
            ]);
        let request = apply_history_range_params(
            request,
            input.cursor.as_deref(),
            input.oldest.as_deref(),
            input.latest.as_deref(),
            input.inclusive,
        );
        let json = self.send_request("conversations.replies", request).await?;
        let parsed: RawHistoryResponse =
            serde_json::from_value(json).map_err(|e| SlackError::UnexpectedShape {
                message: format!("unexpected conversations.replies shape: {e}"),
            })?;
        let resolve_mode = input.resolve_users.unwrap_or_default();
        let mut messages = normalize_messages(
            parsed.messages,
            &input.channel_id,
            SlackSourceKind::ThreadReply,
            input.order.unwrap_or_default(),
        );
        let users = if matches!(resolve_mode, SlackUserResolutionMode::ResolveUsers) {
            self.resolve_users_for_messages(&messages, input.auth).await
        } else {
            Vec::new()
        };
        attach_user_names(&mut messages, &users);
        let next_cursor = normalize_cursor(parsed.response_metadata.as_ref());
        let has_more = parsed.has_more || next_cursor.is_some();
        let sources = dedupe_sources(
            messages
                .iter()
                .map(|message| SlackSource {
                    kind: SlackSourceKind::ThreadReply,
                    reference: message.source_ref.clone(),
                    permalink: message.permalink.clone(),
                    channel_id: Some(message.channel_id.clone()),
                    message_ts: Some(message.ts.clone()),
                    thread_ts: message.thread_ts.clone(),
                })
                .collect(),
        );
        let count = messages.len();
        Ok(SlackOutput {
            conversations: Vec::new(),
            messages,
            users,
            next_cursor,
            has_more,
            sources,
            message: format!(
                "Retrieved {count} thread message(s) from {} at {}",
                input.channel_id, input.thread_ts
            ),
        })
    }

    async fn resolve_users(&self, input: ResolveUsersInput) -> Result<SlackOutput> {
        let mut requested: Vec<String> = input
            .user_ids
            .into_iter()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
        requested.sort();
        requested.dedup();
        if requested.is_empty() {
            return Err(SlackError::InvalidArgument {
                message: "user_ids must contain at least one non-empty id".to_string(),
            }
            .into());
        }
        let users = self.resolve_users_by_ids(&requested, input.auth).await;
        let sources = dedupe_sources(
            users
                .iter()
                .map(|user| SlackSource {
                    kind: SlackSourceKind::User,
                    reference: format!("slack://user/{}", user.id),
                    permalink: None,
                    channel_id: None,
                    message_ts: None,
                    thread_ts: None,
                })
                .collect(),
        );
        let count = users.len();
        Ok(SlackOutput {
            conversations: Vec::new(),
            messages: Vec::new(),
            users,
            next_cursor: None,
            has_more: false,
            sources,
            message: format!("Resolved {count} user(s)"),
        })
    }

    async fn search_messages(&self, input: SearchMessagesInput) -> Result<SlackOutput> {
        if input.query.trim().is_empty() {
            return Err(SlackError::InvalidArgument {
                message: "query must not be empty".to_string(),
            }
            .into());
        }
        let (token, _token_kind) = self.auth.select_token(input.auth, true)?;
        let count = input.count.unwrap_or(20).clamp(1, 100);
        let page = input.page.unwrap_or(1).max(1);
        let sort = match input.sort.unwrap_or_default() {
            SlackSearchSort::Score => "score",
            SlackSearchSort::Timestamp => "timestamp",
        };
        let sort_dir = match input.direction.unwrap_or_default() {
            SlackSearchDirection::Asc => "asc",
            SlackSearchDirection::Desc => "desc",
        };
        let request = self.authorized_get(token, "search.messages")?.query(&[
            ("query", input.query.as_str()),
            ("count", &count.to_string()),
            ("page", &page.to_string()),
            ("sort", sort),
            ("sort_dir", sort_dir),
        ]);
        let json = self.send_request("search.messages", request).await?;
        let parsed: RawSearchMessagesResponse =
            serde_json::from_value(json).map_err(|e| SlackError::UnexpectedShape {
                message: format!("unexpected search.messages shape: {e}"),
            })?;
        let resolve_mode = input.resolve_users.unwrap_or_default();
        let mut messages: Vec<SlackMessageSummary> = parsed
            .messages
            .matches
            .into_iter()
            .filter_map(normalize_search_message)
            .collect();
        let users = if matches!(resolve_mode, SlackUserResolutionMode::ResolveUsers) {
            self.resolve_users_for_messages(&messages, input.auth).await
        } else {
            Vec::new()
        };
        attach_user_names(&mut messages, &users);
        let next_cursor = match parsed.messages.pagination {
            Some(ref pagination) => {
                let page = pagination.page.unwrap_or(1);
                let page_count = pagination.page_count.unwrap_or(page);
                if page < page_count {
                    Some((page + 1).to_string())
                } else {
                    None
                }
            }
            None => None,
        };
        let has_more = next_cursor.is_some();
        let sources = dedupe_sources(
            messages
                .iter()
                .map(|message| SlackSource {
                    kind: SlackSourceKind::SearchMatch,
                    reference: message.source_ref.clone(),
                    permalink: message.permalink.clone(),
                    channel_id: Some(message.channel_id.clone()),
                    message_ts: Some(message.ts.clone()),
                    thread_ts: message.thread_ts.clone(),
                })
                .collect(),
        );
        let count = messages.len();
        Ok(SlackOutput {
            conversations: Vec::new(),
            messages,
            users,
            next_cursor,
            has_more,
            sources,
            message: format!("Retrieved {count} search match(es)"),
        })
    }

    async fn resolve_users_for_messages(
        &self,
        messages: &[SlackMessageSummary],
        auth: Option<SlackAuthPreference>,
    ) -> Vec<SlackUserSummary> {
        let user_ids: Vec<String> = messages
            .iter()
            .filter_map(|message| message.user_id.clone())
            .collect();
        self.resolve_users_by_ids(&user_ids, auth).await
    }

    async fn resolve_users_by_ids(
        &self,
        user_ids: &[String],
        auth: Option<SlackAuthPreference>,
    ) -> Vec<SlackUserSummary> {
        let unique_ids: Vec<String> = user_ids
            .iter()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if unique_ids.is_empty() {
            return Vec::new();
        }
        let token = match self.auth.select_token(auth, false) {
            Ok((token, _)) => token,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "Slack user resolution skipped due to missing token configuration"
                );
                return Vec::new();
            }
        };
        let mut users = Vec::new();
        for user_id in unique_ids {
            match self.get_user_info(token, &user_id).await {
                Ok(user) => users.push(user),
                Err(err) => {
                    tracing::warn!(
                        user_id = %user_id,
                        error = %err,
                        "Slack user resolution failed for one user; continuing"
                    );
                }
            }
        }
        users
    }

    async fn get_user_info(
        &self,
        token: &str,
        user_id: &str,
    ) -> std::result::Result<SlackUserSummary, SlackError> {
        if user_id.trim().is_empty() {
            return Err(SlackError::InvalidArgument {
                message: "user_id must not be empty".to_string(),
            });
        }
        let request = self
            .authorized_get(token, "users.info")?
            .query(&[("user", user_id)]);
        let json = self.send_request("users.info", request).await?;
        let parsed: RawUserInfoResponse =
            serde_json::from_value(json).map_err(|e| SlackError::UnexpectedShape {
                message: format!("unexpected users.info shape: {e}"),
            })?;
        Ok(map_user(parsed.user))
    }
}

fn map_conversation_kind(raw: &RawConversation) -> SlackConversationKind {
    if raw.is_im {
        SlackConversationKind::Im
    } else if raw.is_mpim {
        SlackConversationKind::Mpim
    } else if raw.is_group || raw.is_private {
        SlackConversationKind::PrivateChannel
    } else {
        SlackConversationKind::PublicChannel
    }
}

fn normalize_messages(
    raw_messages: Vec<RawMessage>,
    channel_id: &str,
    source_kind: SlackSourceKind,
    order: SlackHistoryOrder,
) -> Vec<SlackMessageSummary> {
    let mut messages: Vec<SlackMessageSummary> = raw_messages
        .into_iter()
        .filter_map(|raw| normalize_message(raw, channel_id, source_kind.clone()))
        .collect();
    if matches!(order, SlackHistoryOrder::OldestFirst) {
        messages.sort_by(|left, right| {
            parse_ts_numeric(&left.ts)
                .partial_cmp(&parse_ts_numeric(&right.ts))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    messages
}

fn normalize_message(
    raw: RawMessage,
    channel_id: &str,
    source_kind: SlackSourceKind,
) -> Option<SlackMessageSummary> {
    let ts = raw.ts?;
    let source_ref = message_reference(channel_id, &ts);
    let permalink = raw.permalink.clone();
    let mut user_name = raw.username;
    if user_name.as_deref().is_some_and(str::is_empty) {
        user_name = None;
    }
    let thread_ts = raw.thread_ts.clone();
    let message = SlackMessageSummary {
        channel_id: channel_id.to_string(),
        ts,
        thread_ts,
        user_id: raw.user,
        user_name,
        text: raw.text.unwrap_or_default(),
        subtype: raw.subtype,
        source_ref,
        permalink,
    };
    if matches!(source_kind, SlackSourceKind::ThreadReply)
        && message.thread_ts.is_none()
        && message.ts.is_empty()
    {
        return None;
    }
    Some(message)
}

fn normalize_search_message(raw: RawSearchMessage) -> Option<SlackMessageSummary> {
    let channel_id = raw.channel.id;
    let ts = raw.ts?;
    Some(SlackMessageSummary {
        channel_id: channel_id.clone(),
        ts: ts.clone(),
        thread_ts: raw.thread_ts,
        user_id: raw.user,
        user_name: raw.username,
        text: raw.text.unwrap_or_default(),
        subtype: raw.subtype,
        source_ref: message_reference(&channel_id, &ts),
        permalink: raw.permalink,
    })
}

fn map_user(raw: RawUser) -> SlackUserSummary {
    let (display_name, real_name, email) = match raw.profile {
        Some(profile) => (profile.display_name, profile.real_name, profile.email),
        None => (None, None, None),
    };
    SlackUserSummary {
        id: raw.id,
        name: raw.name,
        display_name,
        real_name,
        is_bot: raw.is_bot,
        is_deleted: raw.deleted,
        email,
    }
}

fn attach_user_names(messages: &mut [SlackMessageSummary], users: &[SlackUserSummary]) {
    let user_name_by_id: HashMap<&str, &str> = users
        .iter()
        .filter_map(|user| {
            user.display_name
                .as_deref()
                .or(user.real_name.as_deref())
                .or(user.name.as_deref())
                .map(|name| (user.id.as_str(), name))
        })
        .collect();
    for message in messages {
        if let Some(user_id) = message.user_id.as_deref()
            && let Some(name) = user_name_by_id.get(user_id)
        {
            message.user_name = Some((*name).to_string());
        }
    }
}

fn normalize_cursor(meta: Option<&RawResponseMetadata>) -> Option<String> {
    meta.and_then(|m| m.next_cursor.as_deref())
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
        .map(ToOwned::to_owned)
}

fn apply_history_range_params<'a>(
    mut request: reqwest::RequestBuilder,
    cursor: Option<&'a str>,
    oldest: Option<&'a str>,
    latest: Option<&'a str>,
    inclusive: Option<bool>,
) -> reqwest::RequestBuilder {
    if let Some(cursor) = cursor {
        request = request.query(&[("cursor", cursor)]);
    }
    if let Some(oldest) = oldest {
        request = request.query(&[("oldest", oldest)]);
    }
    if let Some(latest) = latest {
        request = request.query(&[("latest", latest)]);
    }
    if let Some(inclusive) = inclusive {
        request = request.query(&[("inclusive", inclusive)]);
    }
    request
}

fn message_reference(channel_id: &str, ts: &str) -> String {
    let compact_ts = ts.replace('.', "");
    format!("slack://channel/{channel_id}/p{compact_ts}")
}

fn conversation_types_param(kinds: &[SlackConversationKind]) -> String {
    if kinds.is_empty() {
        return "public_channel,private_channel,im,mpim".to_string();
    }
    let mut set = HashSet::new();
    let mut ordered = Vec::new();
    for kind in kinds {
        let value = kind.as_api_type();
        if set.insert(value) {
            ordered.push(value);
        }
    }
    ordered.join(",")
}

fn parse_ts_numeric(value: &str) -> f64 {
    value.parse::<f64>().unwrap_or(0.0)
}

fn dedupe_sources(sources: Vec<SlackSource>) -> Vec<SlackSource> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for source in sources {
        let key = format!(
            "{}|{}",
            source.reference,
            source.permalink.as_deref().unwrap_or("")
        );
        if seen.insert(key) {
            out.push(source);
        }
    }
    out
}

fn should_warn_on_insecure_base_url(base_url: &str) -> bool {
    if base_url.starts_with("https://") {
        return false;
    }
    if let Ok(parsed) = reqwest::Url::parse(base_url)
        && let Some(host) = parsed.host_str()
    {
        let normalized = host.to_ascii_lowercase();
        if normalized == "localhost" || normalized == "127.0.0.1" || normalized == "::1" {
            return false;
        }
    }
    true
}

fn map_slack_api_error(method: &'static str, error: String) -> SlackError {
    let class = if is_auth_error_code(&error) {
        SlackApiErrorClass::Configuration
    } else if is_invalid_argument_error_code(&error) {
        SlackApiErrorClass::InvalidArgument
    } else {
        SlackApiErrorClass::ToolExecution
    };
    SlackError::ApiError {
        method,
        error,
        class,
    }
}

fn is_auth_error_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_auth"
            | "not_authed"
            | "account_inactive"
            | "token_revoked"
            | "missing_scope"
            | "not_allowed_token_type"
    )
}

fn is_invalid_argument_error_code(code: &str) -> bool {
    matches!(
        code,
        "channel_not_found"
            | "thread_not_found"
            | "user_not_found"
            | "message_not_found"
            | "messages_not_found"
            | "invalid_cursor"
            | "invalid_ts_latest"
            | "invalid_ts_oldest"
            | "invalid_args"
            | "invalid_arguments"
            | "invalid_arg_name"
            | "invalid_array_arg"
            | "invalid_query"
            | "not_in_channel"
    )
}

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

pub struct SlackTool {
    client: SlackClient,
}

impl Default for SlackTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SlackTool {
    pub fn new() -> Self {
        Self {
            client: SlackClient::new(),
        }
    }
}

#[async_trait]
impl BamlTool for SlackTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "slack";
    type OpenInput = ();
    type Input = SlackInput;
    type Output = SlackOutput;

    fn description(&self) -> &'static str {
        "Read-only Slack access: list conversations, fetch history/thread replies, resolve users, and search messages."
    }

    #[tracing::instrument(skip(self), fields(action))]
    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let action = match &args {
            SlackInput::ListConversations(_) => "ListConversations",
            SlackInput::GetConversationHistory(_) => "GetConversationHistory",
            SlackInput::GetThreadReplies(_) => "GetThreadReplies",
            SlackInput::ResolveUsers(_) => "ResolveUsers",
            SlackInput::SearchMessages(_) => "SearchMessages",
        };
        tracing::Span::current().record("action", action);
        match args {
            SlackInput::ListConversations(input) => self.client.list_conversations(input).await,
            SlackInput::GetConversationHistory(input) => {
                self.client.get_conversation_history(input).await
            }
            SlackInput::GetThreadReplies(input) => self.client.get_thread_replies(input).await,
            SlackInput::ResolveUsers(input) => self.client.resolve_users(input).await,
            SlackInput::SearchMessages(input) => self.client.search_messages(input).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Metadata registration
// ---------------------------------------------------------------------------

pub fn slack_metadata() -> ToolFunctionMetadata {
    let (name, class_name) =
        parse_tool_name_and_class("support/slack").expect("support/slack is a compile-time const");
    let baml_decl = [
        SlackAuthPreference::baml_decl(),
        SlackConversationKind::baml_decl(),
        SlackHistoryOrder::baml_decl(),
        SlackUserResolutionMode::baml_decl(),
        SlackSearchSort::baml_decl(),
        SlackSearchDirection::baml_decl(),
        ListConversationsInput::baml_decl(),
        GetConversationHistoryInput::baml_decl(),
        GetThreadRepliesInput::baml_decl(),
        ResolveUsersInput::baml_decl(),
        SearchMessagesInput::baml_decl(),
        SlackInput::baml_decl(),
        SlackSourceKind::baml_decl(),
        SlackSource::baml_decl(),
        SlackConversationSummary::baml_decl(),
        SlackUserSummary::baml_decl(),
        SlackMessageSummary::baml_decl(),
        SlackOutput::baml_decl(),
    ]
    .join("\n\n");

    TypeBasedMetadataBuilder::<(), SlackInput, SlackOutput>::new(
        name,
        class_name,
        "Read-only Slack integration for conversation retrieval and source-backed analysis."
            .to_string(),
    )
    .with_baml_decl(baml_decl)
    .with_tags(vec![
        "support".to_string(),
        "slack".to_string(),
        "read".to_string(),
    ])
    .with_access(ToolAccess::Read)
    .with_secrets(vec![
        ToolSecretRequirement {
            name: "SLACK_BOT_TOKEN".to_string(),
            description: "Slack bot token (xoxb-...)".to_string(),
            reason: "Required for read access to conversations/history for bot-authorized installs"
                .to_string(),
        },
        ToolSecretRequirement {
            name: "SLACK_USER_TOKEN".to_string(),
            description: "Slack user token (xoxp-...)".to_string(),
            reason: "Required for user-scoped reads such as message search and user-limited access"
                .to_string(),
        },
    ])
    .build_metadata()
}

fn slack_build() -> Result<Arc<dyn ToolHandler>> {
    create_tool_handler(SlackTool::new()).map(|(_, handler)| handler)
}

register_tool!(slack_metadata, slack_build);

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use baml_rt_core::BamlRtError;
    use baml_rt_tools::tools::BamlTool;
    use serde_json::json;
    use test_support::common::TempEnvVar;

    use super::{
        BASE_URL, GetConversationHistoryInput, ListConversationsInput, RetryAfter,
        SlackApiErrorClass, SlackClient, SlackConversationKind, SlackInput, SlackTool,
        backoff_delay, map_slack_api_error, should_warn_on_insecure_base_url,
    };

    fn slack_env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[test]
    fn backoff_delay_is_capped() {
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(3), Duration::from_millis(4000));
        assert_eq!(backoff_delay(4), Duration::from_millis(5000));
    }

    #[test]
    fn parse_retry_after_header() {
        let header = reqwest::header::HeaderValue::from_static("5");
        assert!(matches!(
            SlackClient::parse_retry_after(Some(&header)),
            RetryAfter::Seconds(5)
        ));
        let header = reqwest::header::HeaderValue::from_static("n/a");
        assert!(matches!(
            SlackClient::parse_retry_after(Some(&header)),
            RetryAfter::Unknown(value) if value == "n/a"
        ));
        assert!(matches!(
            SlackClient::parse_retry_after(None),
            RetryAfter::Missing
        ));
    }

    #[tokio::test]
    async fn slack_base_url_defaults_to_constant() {
        let _guard = slack_env_lock().lock().await;
        let _env = TempEnvVar::remove("SLACK_API_BASE_URL");
        let client = SlackClient::new();
        assert_eq!(client.base_url(), BASE_URL);
    }

    #[tokio::test]
    async fn slack_base_url_uses_override_and_trims_trailing_slash() {
        let _guard = slack_env_lock().lock().await;
        let _env = TempEnvVar::set("SLACK_API_BASE_URL", " https://mock.slack.local/api/ ");
        let client = SlackClient::new();
        assert_eq!(client.base_url(), "https://mock.slack.local/api");
    }

    #[tokio::test]
    async fn slack_base_url_is_bound_at_client_creation() {
        let _guard = slack_env_lock().lock().await;
        let _env_unset = TempEnvVar::remove("SLACK_API_BASE_URL");
        let client = SlackClient::new();
        let _env_override = TempEnvVar::set("SLACK_API_BASE_URL", "https://later-change.local");
        assert_eq!(client.base_url(), BASE_URL);
    }

    #[test]
    fn slack_insecure_base_url_warning_policy_skips_localhost() {
        assert!(!should_warn_on_insecure_base_url(
            "http://127.0.0.1:8080/api"
        ));
        assert!(!should_warn_on_insecure_base_url(
            "http://localhost:8080/api"
        ));
        assert!(should_warn_on_insecure_base_url(
            "http://169.254.169.254/latest"
        ));
    }

    #[test]
    fn slack_api_error_mapping_routes_invalid_argument() {
        let err = map_slack_api_error("conversations.history", "channel_not_found".to_string());
        match err {
            super::SlackError::ApiError { class, .. } => {
                assert!(matches!(class, SlackApiErrorClass::InvalidArgument));
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_token_maps_to_configuration_error() {
        let _guard = slack_env_lock().lock().await;
        let _env_bot = TempEnvVar::remove("SLACK_BOT_TOKEN");
        let _env_user = TempEnvVar::remove("SLACK_USER_TOKEN");
        let tool = SlackTool::new();
        let err = tool
            .execute(SlackInput::ListConversations(ListConversationsInput {
                kinds: vec![SlackConversationKind::PublicChannel],
                cursor: None,
                limit: None,
                exclude_archived: None,
                include_num_members: None,
                auth: None,
            }))
            .await
            .expect_err("expected missing token error");
        assert!(
            matches!(err, BamlRtError::Configuration(_)),
            "expected configuration error, got {err:?}"
        );
    }

    #[derive(Clone, Default)]
    struct MockState {
        hits: Arc<tokio::sync::Mutex<Vec<String>>>,
        rate_limit_hits: Arc<AtomicUsize>,
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

    #[tokio::test]
    async fn pagination_cursor_is_returned_for_conversations_list() {
        let _guard = slack_env_lock().lock().await;
        let state = MockState::default();
        let app = Router::new()
            .route(
                "/api/conversations.list",
                get({
                    let state = state.clone();
                    move |headers: HeaderMap| {
                        let state = state.clone();
                        async move {
                            let auth = headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default()
                                .to_string();
                            state
                                .push_hit(format!("GET /api/conversations.list auth={auth}"))
                                .await;
                            Json(json!({
                                "ok": true,
                                "channels": [
                                    { "id": "C123", "name": "eng", "is_private": false, "is_archived": false }
                                ],
                                "response_metadata": { "next_cursor": "cursor-2" }
                            }))
                        }
                    }
                }),
            )
            .with_state(state.clone());
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);

        let tool = SlackTool::new();
        let output = tool
            .execute(SlackInput::ListConversations(ListConversationsInput {
                kinds: vec![SlackConversationKind::PublicChannel],
                cursor: None,
                limit: Some(1),
                exclude_archived: Some(true),
                include_num_members: Some(false),
                auth: None,
            }))
            .await
            .expect("list conversations should succeed");
        assert_eq!(output.next_cursor.as_deref(), Some("cursor-2"));
        assert!(output.has_more);
        assert_eq!(output.conversations.len(), 1);
        assert!(
            output
                .sources
                .iter()
                .any(|source| source.reference == "slack://channel/C123")
        );
    }

    #[tokio::test]
    async fn api_error_channel_not_found_maps_to_invalid_argument() {
        let _guard = slack_env_lock().lock().await;
        let app = Router::new().route(
            "/api/conversations.history",
            get(|| async {
                Json(json!({
                    "ok": false,
                    "error": "channel_not_found"
                }))
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);
        let tool = SlackTool::new();
        let err = tool
            .execute(SlackInput::GetConversationHistory(
                GetConversationHistoryInput {
                    channel_id: "C404".to_string(),
                    cursor: None,
                    limit: None,
                    oldest: None,
                    latest: None,
                    inclusive: None,
                    order: None,
                    resolve_users: None,
                    auth: None,
                },
            ))
            .await
            .expect_err("expected invalid argument mapping");
        assert!(
            matches!(err, BamlRtError::InvalidArgument(_)),
            "expected InvalidArgument, got {err:?}"
        );
    }

    #[tokio::test]
    async fn rate_limit_retry_uses_retry_after_and_recovers() {
        let _guard = slack_env_lock().lock().await;
        let state = MockState::default();
        let app = Router::new().route(
            "/api/conversations.list",
            get({
                let state = state.clone();
                move || {
                    let state = state.clone();
                    async move {
                        let attempt = state.rate_limit_hits.fetch_add(1, Ordering::SeqCst) + 1;
                        state
                            .push_hit(format!("GET /api/conversations.list attempt={attempt}"))
                            .await;
                        if attempt == 1 {
                            (
                                StatusCode::TOO_MANY_REQUESTS,
                                [("retry-after", "0")],
                                json!({ "ok": false, "error": "ratelimited" }).to_string(),
                            )
                        } else {
                            (
                                StatusCode::OK,
                                [("content-type", "application/json")],
                                json!({
                                    "ok": true,
                                    "channels": [],
                                    "response_metadata": { "next_cursor": "" }
                                })
                                .to_string(),
                            )
                        }
                    }
                }
            }),
        );
        let base_url = start_mock_server(app).await.expect("start server");
        let _env_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
        let _env_base = TempEnvVar::set("SLACK_API_BASE_URL", &base_url);
        let tool = SlackTool::new();
        let output = tool
            .execute(SlackInput::ListConversations(ListConversationsInput {
                kinds: vec![SlackConversationKind::PublicChannel],
                cursor: None,
                limit: None,
                exclude_archived: None,
                include_num_members: None,
                auth: None,
            }))
            .await
            .expect("tool should recover after one 429");
        assert_eq!(output.conversations.len(), 0);
        assert_eq!(state.rate_limit_hits.load(Ordering::SeqCst), 2);
        let hits = state.snapshot().await;
        assert_eq!(hits.len(), 2, "expected two hits due to retry: {hits:?}");
    }
}
