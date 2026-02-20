//! Notion tools — `support/notionSearchPages`, `support/notionGetPage`, `support/notionGetPageBlocks`.
//!
//! Provides read-only access to the Notion REST API.
//! Supports optional block processing controls:
//! - `raw_blocks`: render mode (raw skips Notable lines / Missing info hints).
//! - `max_depth`: limit child block expansion depth (0 disables expansion).

use std::{collections::VecDeque, fmt, time::Duration};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_derive_core::BamlType as BamlTypeTrait;
use baml_rt_core::{BamlRtError, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    bundles::Support,
    register_tool_metadata, spans,
    tools::{BamlTool, ToolAccess, ToolFunctionMetadata, ToolSecretRequirement},
};

/// Notion REST API base URL.
pub const BASE_URL: &str = "https://api.notion.com/v1";
/// Notion API version header value.
pub const NOTION_VERSION: &str = "2025-09-03";
const MAX_BLOCK_DEPTH: u32 = 10;
const MAX_RATE_LIMIT_RETRIES: usize = 3;
const RATE_LIMIT_BASE_DELAY_MS: u64 = 500;
const RATE_LIMIT_MAX_DELAY_MS: u64 = 5_000;
const MAX_BLOCK_PAGES: usize = 10;

fn backoff_delay(retries: usize) -> Duration {
    let shift = u32::try_from(retries).unwrap_or(u32::MAX);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let backoff = RATE_LIMIT_BASE_DELAY_MS.saturating_mul(multiplier);
    Duration::from_millis(backoff.min(RATE_LIMIT_MAX_DELAY_MS))
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NotionSearchPagesInput {
    pub query: Option<String>,
    pub start_cursor: Option<String>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NotionGetPageInput {
    pub page_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NotionGetPageBlocksInput {
    pub block_id: String,
    pub start_cursor: Option<String>,
    pub page_size: Option<u32>,
    /// Render mode for blocks (raw skips Notable lines / Missing info hints).
    pub raw_blocks: Option<BlockRenderMode>,
    /// Max child block depth to expand (0 = no child expansion).
    pub max_depth: Option<u32>,
}

/// Input for the Notion tool as per-action typed variants.
///
/// `deny_unknown_fields` on each variant ensures untagged routing rejects
/// mismatched fields. An empty object `{}` resolves to `SearchPages` (all
/// fields optional), which is intentional: a bare search is a safe default.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[baml(union)]
#[serde(untagged)]
#[ts(export)]
pub enum NotionInput {
    SearchPages(NotionSearchPagesInput),
    GetPage(NotionGetPageInput),
    GetPageBlocks(NotionGetPageBlocksInput),
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct NotionPageSummary {
    pub id: String,
    pub title: String,
    pub url: String,
    pub last_edited_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct NotionBlockSummary {
    pub id: String,
    pub block_type: String,
    pub text: Option<String>,
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct NotionSource {
    pub page_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct NotionOutput {
    pub pages: Vec<NotionPageSummary>,
    pub blocks: Vec<NotionBlockSummary>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub sources: Vec<NotionSource>,
    pub message: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize, JsonSchema, TS, BamlType)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum BlockRenderMode {
    Raw,
    #[default]
    Enriched,
}

impl BlockRenderMode {
    pub const fn is_raw(self) -> bool {
        matches!(self, Self::Raw)
    }
}

impl From<bool> for BlockRenderMode {
    fn from(value: bool) -> Self {
        if value { Self::Raw } else { Self::Enriched }
    }
}

impl From<BlockRenderMode> for bool {
    fn from(value: BlockRenderMode) -> Self {
        value.is_raw()
    }
}

impl<'de> Deserialize<'de> for BlockRenderMode {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ModeVisitor;

        impl<'de> serde::de::Visitor<'de> for ModeVisitor {
            type Value = BlockRenderMode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("bool or string (raw/enriched)")
            }

            fn visit_bool<E>(self, v: bool) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BlockRenderMode::from(v))
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match v {
                    "raw" | "RAW" | "Raw" => Ok(BlockRenderMode::Raw),
                    "enriched" | "ENRICHED" | "Enriched" => Ok(BlockRenderMode::Enriched),
                    _ => Err(E::custom("expected raw or enriched")),
                }
            }

            fn visit_string<E>(self, v: String) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(&v)
            }
        }

        deserializer.deserialize_any(ModeVisitor)
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum NotionError {
    #[error("Notion HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("Notion API authentication failed ({status}): {body}")]
    Unauthorized {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Notion resource not found (404): {body}")]
    NotFound { body: String },

    #[error("Notion rate limit exceeded (429), retry after {retry_after}: {body}")]
    RateLimited {
        body: String,
        retry_after: RetryAfter,
    },

    #[error("Notion API returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Failed to deserialize Notion response")]
    Deserialize(#[source] reqwest::Error),

    #[error("Unexpected Notion response shape: {message}")]
    UnexpectedShape { message: String },

    #[error("NOTION_API_TOKEN environment variable not set")]
    MissingApiKey,

    #[error("Invalid Notion id '{id}'")]
    InvalidId { id: String },

    #[error("Invalid Notion header value: {message}")]
    InvalidHeader { message: String },

    #[error("Failed to clone Notion request")]
    RequestClone,
}

impl From<NotionError> for BamlRtError {
    fn from(err: NotionError) -> Self {
        match &err {
            NotionError::MissingApiKey
            | NotionError::Unauthorized { .. }
            | NotionError::InvalidHeader { .. } => BamlRtError::Configuration(err.to_string()),
            NotionError::InvalidId { .. } | NotionError::NotFound { .. } => {
                BamlRtError::InvalidArgument(err.to_string())
            }
            NotionError::Http(_)
            | NotionError::RateLimited { .. }
            | NotionError::Api { .. }
            | NotionError::Deserialize(_)
            | NotionError::UnexpectedShape { .. }
            | NotionError::RequestClone => BamlRtError::ToolExecution(err.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Client helpers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct NotionClient {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
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

impl fmt::Display for RetryAfter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetryAfter::Seconds(seconds) => write!(f, "{seconds}s"),
            RetryAfter::Unknown(raw) => write!(f, "unknown({raw})"),
            RetryAfter::Missing => write!(f, "missing"),
        }
    }
}

impl NotionClient {
    fn new() -> Self {
        let api_key = std::env::var("NOTION_API_TOKEN").ok();
        let base_url = Self::resolve_base_url();
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url,
        }
    }

    fn resolve_base_url() -> String {
        std::env::var("NOTION_API_BASE_URL")
            .ok()
            .map(|raw| raw.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| BASE_URL.to_string())
    }

    fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    fn api_key(&self) -> std::result::Result<&str, NotionError> {
        self.api_key.as_deref().ok_or(NotionError::MissingApiKey)
    }

    fn normalize_id(id: &str) -> std::result::Result<String, NotionError> {
        let cleaned: String = id.chars().filter(|c| *c != '-').collect();
        if cleaned.len() != 32 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(NotionError::InvalidId { id: id.to_string() });
        }
        Ok(format!(
            "{}-{}-{}-{}-{}",
            &cleaned[0..8],
            &cleaned[8..12],
            &cleaned[12..16],
            &cleaned[16..20],
            &cleaned[20..32]
        ))
    }

    fn auth_headers(
        &self,
        api_key: &str,
    ) -> std::result::Result<reqwest::header::HeaderMap, NotionError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {api_key}")
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|e| NotionError::InvalidHeader {
                    message: e.to_string(),
                })?,
        );
        headers.insert(
            "Notion-Version",
            NOTION_VERSION
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|e| NotionError::InvalidHeader {
                    message: e.to_string(),
                })?,
        );
        Ok(headers)
    }

    async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, NotionError> {
        let request = request.build().map_err(NotionError::Http)?;
        let url = request.url().to_string();
        let span = spans::notion_request(&url);
        let _guard = span.enter();
        let mut retries = 0usize;

        loop {
            let req = request.try_clone().ok_or(NotionError::RequestClone)?;
            let resp = self.client.execute(req).await.map_err(NotionError::Http)?;

            let status = resp.status();
            if !status.is_success() {
                let retry_after = Self::parse_retry_after(resp.headers().get("retry-after"));
                let body = resp.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    && retries < MAX_RATE_LIMIT_RETRIES
                {
                    let delay = retry_after
                        .as_duration()
                        .unwrap_or_else(|| backoff_delay(retries));
                    tracing::warn!(
                        retries = retries + 1,
                        retry_after = %retry_after,
                        "Notion rate limit hit; backing off"
                    );
                    retries += 1;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(match status {
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                        NotionError::Unauthorized { status, body }
                    }
                    reqwest::StatusCode::NOT_FOUND => NotionError::NotFound { body },
                    reqwest::StatusCode::TOO_MANY_REQUESTS => {
                        NotionError::RateLimited { body, retry_after }
                    }
                    _ => NotionError::Api { status, body },
                });
            }

            return resp.json().await.map_err(NotionError::Deserialize);
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

    async fn search_pages(
        &self,
        api_key: &str,
        query: Option<&str>,
        start_cursor: Option<&str>,
        page_size: Option<u32>,
    ) -> Result<NotionOutput> {
        let span = spans::notion_search_pages(query.map(|q| q.len()), page_size);
        let _guard = span.enter();
        let base_url = self.base_url();
        let mut body = serde_json::Map::new();
        if let Some(q) = query {
            body.insert(
                "query".to_string(),
                serde_json::Value::String(q.to_string()),
            );
        }
        body.insert(
            "filter".to_string(),
            serde_json::json!({"property": "object", "value": "page"}),
        );
        if let Some(cursor) = start_cursor {
            body.insert(
                "start_cursor".to_string(),
                serde_json::Value::String(cursor.to_string()),
            );
        }
        if let Some(size) = page_size {
            body.insert("page_size".to_string(), serde_json::json!(size));
        }

        let json = self
            .send_request(
                self.client
                    .post(format!("{base_url}/search"))
                    .headers(self.auth_headers(api_key)?)
                    .json(&serde_json::Value::Object(body)),
            )
            .await?;

        let (pages, sources) = extract_pages(&json);
        let next_cursor = json
            .get("next_cursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let has_more = json
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let count = pages.len();
        Ok(NotionOutput {
            pages,
            blocks: Vec::new(),
            next_cursor,
            has_more,
            sources,
            message: format!("Found {count} page(s)"),
        })
    }

    async fn get_page(&self, api_key: &str, page_id: &str) -> Result<NotionOutput> {
        let span = spans::notion_get_page(page_id);
        let _guard = span.enter();
        let base_url = self.base_url();
        let normalized = Self::normalize_id(page_id)?;
        let json = self
            .send_request(
                self.client
                    .get(format!("{base_url}/pages/{normalized}"))
                    .headers(self.auth_headers(api_key)?),
            )
            .await?;

        let (pages, sources) = extract_pages(&json);
        let title = pages
            .first()
            .map(|p| p.title.clone())
            .unwrap_or_else(|| "Untitled".to_string());

        Ok(NotionOutput {
            pages,
            blocks: Vec::new(),
            next_cursor: None,
            has_more: false,
            sources,
            message: format!("Page: {title}"),
        })
    }

    async fn get_page_blocks(
        &self,
        api_key: &str,
        block_id: &str,
        start_cursor: Option<&str>,
        page_size: Option<u32>,
        render_mode: BlockRenderMode,
        max_depth: u32,
    ) -> Result<NotionOutput> {
        let span = spans::notion_get_page_blocks(block_id);
        let _guard = span.enter();
        let normalized = Self::normalize_id(block_id)?;
        let (pages, sources, mut blocks, next_cursor, has_more) = self
            .fetch_blocks_all_pages(
                api_key,
                &normalized,
                start_cursor,
                page_size,
                render_mode,
                true,
            )
            .await?;

        let mut visited = std::collections::HashSet::new();
        visited.insert(normalized.clone());
        if max_depth > 0 {
            let child_blocks = self
                .fetch_child_blocks_recursive(
                    api_key,
                    &blocks,
                    &mut visited,
                    max_depth,
                    render_mode,
                )
                .await?;
            blocks.extend(child_blocks);
        }

        if !render_mode.is_raw()
            && let Some(notable) = extract_notable_lines(&blocks)
        {
            blocks.insert(0, notable);
        }

        Ok(NotionOutput {
            pages,
            blocks,
            next_cursor,
            has_more,
            sources,
            message: "Retrieved page blocks".to_string(),
        })
    }

    async fn fetch_page_summary(
        &self,
        api_key: &str,
        page_id: &str,
    ) -> std::result::Result<NotionPageSummary, NotionError> {
        let span = spans::notion_fetch_page_summary(page_id);
        let _guard = span.enter();
        let base_url = self.base_url();
        let normalized = Self::normalize_id(page_id)?;
        let json = self
            .send_request(
                self.client
                    .get(format!("{base_url}/pages/{normalized}"))
                    .headers(self.auth_headers(api_key)?),
            )
            .await?;
        parse_page_summary(&json).ok_or_else(|| NotionError::UnexpectedShape {
            message: "unexpected page shape".to_string(),
        })
    }

    async fn fetch_blocks_page(
        &self,
        api_key: &str,
        block_id: &str,
        start_cursor: Option<&str>,
        page_size: Option<u32>,
        render_mode: BlockRenderMode,
        include_page_summary: bool,
    ) -> Result<(
        Vec<NotionPageSummary>,
        Vec<NotionSource>,
        Vec<NotionBlockSummary>,
        Option<String>,
        bool,
    )> {
        let span = spans::notion_get_page_blocks(block_id);
        let _guard = span.enter();
        let base_url = self.base_url();
        let mut request = self
            .client
            .get(format!("{base_url}/blocks/{block_id}/children"))
            .headers(self.auth_headers(api_key)?);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(cursor) = start_cursor {
            params.push(("start_cursor", cursor.to_string()));
        }
        if let Some(size) = page_size {
            params.push(("page_size", size.to_string()));
        }
        if !params.is_empty() {
            request = request.query(&params);
        }

        let json = self.send_request(request).await?;
        let blocks = extract_blocks(&json, render_mode);
        let next_cursor = json
            .get("next_cursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let has_more = json
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut pages = Vec::new();
        let mut sources = Vec::new();
        if include_page_summary
            && let Some(parent_page_id) = extract_parent_page_id(&json)
            && let Ok(page) = self.fetch_page_summary(api_key, &parent_page_id).await
        {
            sources.push(NotionSource {
                page_id: page.id.clone(),
                url: page.url.clone(),
            });
            pages.push(page);
        }

        Ok((pages, sources, blocks, next_cursor, has_more))
    }

    async fn fetch_blocks_all_pages(
        &self,
        api_key: &str,
        block_id: &str,
        start_cursor: Option<&str>,
        page_size: Option<u32>,
        render_mode: BlockRenderMode,
        include_page_summary: bool,
    ) -> Result<(
        Vec<NotionPageSummary>,
        Vec<NotionSource>,
        Vec<NotionBlockSummary>,
        Option<String>,
        bool,
    )> {
        let mut pages_out = Vec::new();
        let mut sources_out = Vec::new();
        let mut blocks_out = Vec::new();
        let mut cursor = start_cursor.map(|s| s.to_string());
        let mut has_more = true;
        let mut next_cursor = None;
        let mut pages_fetched = 0usize;

        while has_more {
            let (pages, sources, blocks, next, more) = self
                .fetch_blocks_page(
                    api_key,
                    block_id,
                    cursor.as_deref(),
                    page_size,
                    render_mode,
                    include_page_summary && pages_fetched == 0,
                )
                .await?;
            if pages_out.is_empty() {
                pages_out = pages;
            }
            if sources_out.is_empty() {
                sources_out = sources;
            }
            blocks_out.extend(blocks);
            next_cursor = next.clone();
            has_more = more;
            pages_fetched += 1;
            if pages_fetched >= MAX_BLOCK_PAGES {
                if has_more {
                    tracing::warn!(
                        block_id = block_id,
                        max_pages = MAX_BLOCK_PAGES,
                        "Notion blocks pagination truncated"
                    );
                }
                break;
            }
            cursor = next;
        }

        Ok((pages_out, sources_out, blocks_out, next_cursor, has_more))
    }

    async fn fetch_child_blocks_recursive(
        &self,
        api_key: &str,
        blocks: &[NotionBlockSummary],
        visited: &mut std::collections::HashSet<String>,
        max_depth: u32,
        render_mode: BlockRenderMode,
    ) -> Result<Vec<NotionBlockSummary>> {
        const MAX_CHILD_BLOCKS_TOTAL: usize = 2000;
        let mut all_children = Vec::new();
        let mut queue: VecDeque<(String, u32)> = blocks
            .iter()
            .filter(|b| b.has_children)
            .map(|b| (b.id.clone(), 0))
            .collect();

        while let Some((block_id, depth)) = queue.pop_front() {
            if !visited.insert(block_id.clone()) {
                continue;
            }
            let Some(next_depth) = next_depth_for_children(depth, max_depth) else {
                continue;
            };
            let span = spans::notion_fetch_child_blocks(&block_id);
            let _guard = span.enter();
            let (_pages, _sources, child_blocks, _next, _has_more) = self
                .fetch_blocks_all_pages(api_key, &block_id, None, None, render_mode, false)
                .await?;
            for child in child_blocks.iter().filter(|b| b.has_children) {
                if !visited.contains(&child.id) {
                    queue.push_back((child.id.clone(), next_depth));
                }
            }
            all_children.extend(child_blocks);
            if all_children.len() >= MAX_CHILD_BLOCKS_TOTAL {
                tracing::warn!(
                    block_id = block_id,
                    max_blocks = MAX_CHILD_BLOCKS_TOTAL,
                    "Notion child block expansion truncated"
                );
                break;
            }
        }

        Ok(all_children)
    }
}

fn next_depth_for_children(depth: u32, max_depth: u32) -> Option<u32> {
    if depth >= max_depth {
        None
    } else {
        Some(depth + 1)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Mutex, OnceLock},
        time::Duration,
    };

    use reqwest::header::HeaderValue;
    use test_support::common::TempEnvVar;

    use super::{
        BASE_URL, NotionClient, NotionInput, RetryAfter, backoff_delay, next_depth_for_children,
    };

    fn notion_api_base_url_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn next_depth_for_children_respects_max_depth() {
        assert_eq!(next_depth_for_children(0, 0), None);
        assert_eq!(next_depth_for_children(0, 1), Some(1));
        assert_eq!(next_depth_for_children(1, 1), None);
        assert_eq!(next_depth_for_children(1, 2), Some(2));
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
        let header = HeaderValue::from_static("120");
        assert!(matches!(
            NotionClient::parse_retry_after(Some(&header)),
            RetryAfter::Seconds(120)
        ));

        let header = HeaderValue::from_static("n/a");
        assert!(matches!(
            NotionClient::parse_retry_after(Some(&header)),
            RetryAfter::Unknown(value) if value == "n/a"
        ));

        assert!(matches!(
            NotionClient::parse_retry_after(None),
            RetryAfter::Missing
        ));
    }

    #[test]
    fn notion_input_routes_empty_object_to_search_pages() {
        let input: NotionInput = serde_json::from_str("{}").unwrap();
        assert!(matches!(input, NotionInput::SearchPages(_)));
    }

    #[test]
    fn notion_input_routes_query_to_search_pages() {
        let input: NotionInput = serde_json::from_str(r#"{"query":"roadmap"}"#).unwrap();
        assert!(
            matches!(input, NotionInput::SearchPages(ref s) if s.query.as_deref() == Some("roadmap"))
        );
    }

    #[test]
    fn notion_input_routes_page_id_to_get_page() {
        let input: NotionInput = serde_json::from_str(r#"{"page_id":"abc123"}"#).unwrap();
        assert!(matches!(input, NotionInput::GetPage(ref p) if p.page_id == "abc123"));
    }

    #[test]
    fn notion_input_routes_block_id_to_get_page_blocks() {
        let input: NotionInput = serde_json::from_str(r#"{"block_id":"def456"}"#).unwrap();
        assert!(matches!(input, NotionInput::GetPageBlocks(ref b) if b.block_id == "def456"));
    }

    #[test]
    fn notion_input_rejects_mixed_fields() {
        let result = serde_json::from_str::<NotionInput>(r#"{"page_id":"abc","block_id":"def"}"#);
        assert!(
            result.is_err(),
            "mixed page_id + block_id must not match any variant"
        );
    }

    #[test]
    fn notion_base_url_defaults_to_constant() {
        let _guard = notion_api_base_url_test_lock()
            .lock()
            .expect("lock notion base URL test mutex");
        let _env = TempEnvVar::remove("NOTION_API_BASE_URL");
        let client = NotionClient::new();
        assert_eq!(client.base_url(), BASE_URL);
    }

    #[test]
    fn notion_base_url_uses_override_and_trims_trailing_slash() {
        let _guard = notion_api_base_url_test_lock()
            .lock()
            .expect("lock notion base URL test mutex");
        let _env = TempEnvVar::set("NOTION_API_BASE_URL", " https://mock.notion.local/v1/ ");
        let client = NotionClient::new();
        assert_eq!(client.base_url(), "https://mock.notion.local/v1");
    }

    #[test]
    fn notion_base_url_is_bound_at_client_creation() {
        let _guard = notion_api_base_url_test_lock()
            .lock()
            .expect("lock notion base URL test mutex");
        let _env_unset = TempEnvVar::remove("NOTION_API_BASE_URL");
        let client = NotionClient::new();
        let _env_override = TempEnvVar::set("NOTION_API_BASE_URL", "https://mock.notion.local");
        assert_eq!(client.base_url(), BASE_URL);
    }
}

fn extract_notable_lines(blocks: &[NotionBlockSummary]) -> Option<NotionBlockSummary> {
    const MAX_LINES: usize = 12;
    let mut lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for block in blocks {
        let Some(text) = block.text.as_ref() else {
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let keep = matches!(
            block.block_type.as_str(),
            "heading_1"
                | "heading_2"
                | "heading_3"
                | "bulleted_list_item"
                | "numbered_list_item"
                | "to_do"
        );
        if !keep {
            continue;
        }
        let key = trimmed.to_lowercase();
        if seen.insert(key) {
            lines.push(trimmed.to_string());
        }
        if lines.len() >= MAX_LINES {
            break;
        }
    }
    if lines.is_empty() {
        return None;
    }
    let body = lines
        .into_iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(NotionBlockSummary {
        id: "notable-lines".to_string(),
        block_type: "notable_lines".to_string(),
        text: Some(format!("Notable lines:\n{body}")),
        has_children: false,
    })
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

pub struct NotionTool {
    client: NotionClient,
}

impl Default for NotionTool {
    fn default() -> Self {
        Self::new()
    }
}

impl NotionTool {
    pub fn new() -> Self {
        Self {
            client: NotionClient::new(),
        }
    }
}

#[async_trait]
impl BamlTool for NotionTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "notion";
    type OpenInput = ();
    type Input = NotionInput;
    type Output = NotionOutput;

    fn description(&self) -> &'static str {
        "Read-only Notion access: search pages, get page, and retrieve page blocks."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = self.client.api_key()?;
        match args {
            NotionInput::SearchPages(input) => {
                self.client
                    .search_pages(
                        api_key,
                        input.query.as_deref(),
                        input.start_cursor.as_deref(),
                        input.page_size,
                    )
                    .await
            }
            NotionInput::GetPage(input) => self.client.get_page(api_key, &input.page_id).await,
            NotionInput::GetPageBlocks(input) => {
                let render_mode = input.raw_blocks.unwrap_or_default();
                let max_depth = input.max_depth.unwrap_or(2).min(MAX_BLOCK_DEPTH);
                self.client
                    .get_page_blocks(
                        api_key,
                        &input.block_id,
                        input.start_cursor.as_deref(),
                        input.page_size,
                        render_mode,
                        max_depth,
                    )
                    .await
            }
        }
    }
}

pub struct NotionSearchPagesTool {
    client: NotionClient,
}

impl Default for NotionSearchPagesTool {
    fn default() -> Self {
        Self::new()
    }
}

impl NotionSearchPagesTool {
    pub fn new() -> Self {
        Self {
            client: NotionClient::new(),
        }
    }
}

#[async_trait]
impl BamlTool for NotionSearchPagesTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "notionSearchPages";
    type OpenInput = ();
    type Input = NotionSearchPagesInput;
    type Output = NotionOutput;

    fn description(&self) -> &'static str {
        "Search Notion pages by query. Read-only."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = self.client.api_key()?;
        self.client
            .search_pages(
                api_key,
                args.query.as_deref(),
                args.start_cursor.as_deref(),
                args.page_size,
            )
            .await
    }
}

pub struct NotionGetPageTool {
    client: NotionClient,
}

impl Default for NotionGetPageTool {
    fn default() -> Self {
        Self::new()
    }
}

impl NotionGetPageTool {
    pub fn new() -> Self {
        Self {
            client: NotionClient::new(),
        }
    }
}

#[async_trait]
impl BamlTool for NotionGetPageTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "notionGetPage";
    type OpenInput = ();
    type Input = NotionGetPageInput;
    type Output = NotionOutput;

    fn description(&self) -> &'static str {
        "Fetch a Notion page by id. Read-only."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = self.client.api_key()?;
        self.client.get_page(api_key, &args.page_id).await
    }
}

pub struct NotionGetPageBlocksTool {
    client: NotionClient,
}

impl Default for NotionGetPageBlocksTool {
    fn default() -> Self {
        Self::new()
    }
}

impl NotionGetPageBlocksTool {
    pub fn new() -> Self {
        Self {
            client: NotionClient::new(),
        }
    }
}

#[async_trait]
impl BamlTool for NotionGetPageBlocksTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "notionGetPageBlocks";
    type OpenInput = ();
    type Input = NotionGetPageBlocksInput;
    type Output = NotionOutput;

    fn description(&self) -> &'static str {
        "Retrieve Notion block children for a page or block. Read-only."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = self.client.api_key()?;
        let render_mode = args.raw_blocks.unwrap_or_default();
        let max_depth = args.max_depth.unwrap_or(2).min(MAX_BLOCK_DEPTH);
        self.client
            .get_page_blocks(
                api_key,
                &args.block_id,
                args.start_cursor.as_deref(),
                args.page_size,
                render_mode,
                max_depth,
            )
            .await
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn extract_pages(json: &serde_json::Value) -> (Vec<NotionPageSummary>, Vec<NotionSource>) {
    if let Some(results) = json.get("results").and_then(|v| v.as_array()) {
        let mut pages = Vec::new();
        let mut sources = Vec::new();
        for result in results {
            if let Some(page) = parse_page_summary(result) {
                sources.push(NotionSource {
                    page_id: page.id.clone(),
                    url: page.url.clone(),
                });
                pages.push(page);
            }
        }
        return (pages, sources);
    }

    if let Some(page) = parse_page_summary(json) {
        let source = NotionSource {
            page_id: page.id.clone(),
            url: page.url.clone(),
        };
        return (vec![page], vec![source]);
    }

    (Vec::new(), Vec::new())
}

fn parse_page_summary(json: &serde_json::Value) -> Option<NotionPageSummary> {
    if json.get("object")?.as_str()? != "page" {
        return None;
    }
    let id = json.get("id")?.as_str()?.to_string();
    let url = json.get("url")?.as_str()?.to_string();
    let title = extract_page_title(json).unwrap_or_else(|| "Untitled".to_string());
    let last_edited_time = json
        .get("last_edited_time")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(NotionPageSummary {
        id,
        title,
        url,
        last_edited_time,
    })
}

fn extract_page_title(json: &serde_json::Value) -> Option<String> {
    let properties = json.get("properties")?.as_object()?;
    for (_name, prop) in properties {
        if prop.get("type")?.as_str()? == "title" {
            let title = prop.get("title")?.as_array()?;
            let text = title
                .iter()
                .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn extract_blocks(
    json: &serde_json::Value,
    render_mode: BlockRenderMode,
) -> Vec<NotionBlockSummary> {
    let Some(results) = json.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut blocks = Vec::new();
    for block in results {
        if let Some(summary) = parse_block_summary(block) {
            blocks.push(summary);
        }
        if !render_mode.is_raw()
            && let Some(hint) = extract_missing_hint(block)
        {
            blocks.push(hint);
        }
    }
    blocks
}

fn parse_block_summary(json: &serde_json::Value) -> Option<NotionBlockSummary> {
    if json.get("object")?.as_str()? != "block" {
        return None;
    }
    let id = json.get("id")?.as_str()?.to_string();
    let block_type = json.get("type")?.as_str()?.to_string();
    let has_children = json
        .get("has_children")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = extract_block_text(json, &block_type);

    Some(NotionBlockSummary {
        id,
        block_type,
        text,
        has_children,
    })
}

fn extract_block_text(json: &serde_json::Value, block_type: &str) -> Option<String> {
    let block = json.get(block_type)?;
    if block_type == "table_row" {
        let cells = block.get("cells")?.as_array()?;
        let mut parts = Vec::new();
        for cell in cells {
            let text = extract_rich_text(cell);
            parts.push(text);
        }
        let text = parts.join(" | ");
        return if text.trim().is_empty() {
            None
        } else {
            Some(text)
        };
    }
    let rich_text = block.get("rich_text")?.as_array()?;
    let text = rich_text
        .iter()
        .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() { None } else { Some(text) }
}

fn extract_rich_text(value: &serde_json::Value) -> String {
    let Some(array) = value.as_array() else {
        return String::new();
    };
    array
        .iter()
        .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("")
}

fn extract_missing_hint(json: &serde_json::Value) -> Option<NotionBlockSummary> {
    if json.get("object")?.as_str()? != "block" {
        return None;
    }
    let id = json.get("id")?.as_str()?.to_string();
    let block_type = json.get("type")?.as_str()?;
    if block_type != "table_row" {
        return None;
    }
    let row = json.get("table_row")?;
    let cells = row.get("cells")?.as_array()?;
    let mut empty = 0usize;
    let mut total = 0usize;
    for cell in cells {
        total += 1;
        let text = extract_rich_text(cell);
        if text.trim().is_empty() {
            empty += 1;
        }
    }
    if empty == 0 {
        return None;
    }
    Some(NotionBlockSummary {
        id: format!("{id}-missing"),
        block_type: "missing_hint".to_string(),
        text: Some(format!(
            "Missing info: table row has {empty} empty cells out of {total}."
        )),
        has_children: false,
    })
}

fn extract_parent_page_id(json: &serde_json::Value) -> Option<String> {
    let results = json.get("results")?.as_array()?;
    let first = results.first()?;
    let parent = first.get("parent")?.as_object()?;
    if parent.get("type")?.as_str()? == "page_id" {
        return parent
            .get("page_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// Metadata registration (compile-time, for codegen)
// ---------------------------------------------------------------------------

pub fn notion_search_pages_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/notionSearchPages")
        .expect("support/notionSearchPages is a compile-time constant");
    let baml_decl = [
        NotionSearchPagesInput::baml_decl(),
        NotionPageSummary::baml_decl(),
        NotionBlockSummary::baml_decl(),
        NotionSource::baml_decl(),
        NotionOutput::baml_decl(),
    ]
    .join("\n\n");
    TypeBasedMetadataBuilder::<(), NotionSearchPagesInput, NotionOutput>::new(
        name,
        class_name,
        "Search Notion pages by query.".to_string(),
    )
    .with_baml_decl(baml_decl)
    .with_tags(vec![
        "support".to_string(),
        "notion".to_string(),
        "read".to_string(),
    ])
    .with_access(ToolAccess::Read)
    .with_secrets(vec![ToolSecretRequirement {
        name: "NOTION_API_TOKEN".to_string(),
        description: "Notion integration token".to_string(),
        reason: "Required to authenticate with the Notion API".to_string(),
    }])
    .build_metadata()
}

pub fn notion_metadata() -> ToolFunctionMetadata {
    use crate::{
        ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class,
        tool_schema::ts_decl,
    };
    let (name, class_name) = parse_tool_name_and_class("support/notion")
        .expect("support/notion is a compile-time constant");
    let mut extra_ts_decls = Vec::new();
    if let Some(decl) = ts_decl::<BlockRenderMode>() {
        extra_ts_decls.push(decl);
    }
    let baml_decl = [
        NotionSearchPagesInput::baml_decl(),
        NotionGetPageInput::baml_decl(),
        NotionGetPageBlocksInput::baml_decl(),
        BlockRenderMode::baml_decl(),
        NotionInput::baml_decl(),
        NotionPageSummary::baml_decl(),
        NotionBlockSummary::baml_decl(),
        NotionSource::baml_decl(),
        NotionOutput::baml_decl(),
    ]
    .join("\n\n");
    TypeBasedMetadataBuilder::<(), NotionInput, NotionOutput>::new(
        name,
        class_name,
        "Read-only Notion access (search pages, get page, get page blocks).".to_string(),
    )
    .with_baml_decl(baml_decl)
    .with_extra_ts_decls(extra_ts_decls)
    .with_tags(vec![
        "support".to_string(),
        "notion".to_string(),
        "read".to_string(),
    ])
    .with_access(ToolAccess::Read)
    .with_secrets(vec![ToolSecretRequirement {
        name: "NOTION_API_TOKEN".to_string(),
        description: "Notion integration token".to_string(),
        reason: "Required to authenticate with the Notion API".to_string(),
    }])
    .build_metadata()
}

pub fn notion_get_page_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/notionGetPage")
        .expect("support/notionGetPage is a compile-time constant");
    let baml_decl = [
        NotionGetPageInput::baml_decl(),
        NotionPageSummary::baml_decl(),
        NotionBlockSummary::baml_decl(),
        NotionSource::baml_decl(),
        NotionOutput::baml_decl(),
    ]
    .join("\n\n");
    TypeBasedMetadataBuilder::<(), NotionGetPageInput, NotionOutput>::new(
        name,
        class_name,
        "Fetch a Notion page by id.".to_string(),
    )
    .with_baml_decl(baml_decl)
    .with_tags(vec![
        "support".to_string(),
        "notion".to_string(),
        "read".to_string(),
    ])
    .with_access(ToolAccess::Read)
    .with_secrets(vec![ToolSecretRequirement {
        name: "NOTION_API_TOKEN".to_string(),
        description: "Notion integration token".to_string(),
        reason: "Required to authenticate with the Notion API".to_string(),
    }])
    .build_metadata()
}

pub fn notion_get_page_blocks_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/notionGetPageBlocks")
        .expect("support/notionGetPageBlocks is a compile-time constant");
    let baml_decl = [
        BlockRenderMode::baml_decl(),
        NotionGetPageBlocksInput::baml_decl(),
        NotionPageSummary::baml_decl(),
        NotionBlockSummary::baml_decl(),
        NotionSource::baml_decl(),
        NotionOutput::baml_decl(),
    ]
    .join("\n\n");
    TypeBasedMetadataBuilder::<(), NotionGetPageBlocksInput, NotionOutput>::new(
        name,
        class_name,
        "Retrieve Notion block children for a page or block.".to_string(),
    )
    .with_baml_decl(baml_decl)
    .with_tags(vec![
        "support".to_string(),
        "notion".to_string(),
        "read".to_string(),
    ])
    .with_access(ToolAccess::Read)
    .with_secrets(vec![ToolSecretRequirement {
        name: "NOTION_API_TOKEN".to_string(),
        description: "Notion integration token".to_string(),
        reason: "Required to authenticate with the Notion API".to_string(),
    }])
    .build_metadata()
}

register_tool_metadata!(notion_search_pages_metadata);
register_tool_metadata!(notion_get_page_metadata);
register_tool_metadata!(notion_get_page_blocks_metadata);
register_tool_metadata!(notion_metadata);
