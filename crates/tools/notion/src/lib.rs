//! Notion tools — `support/notionSearchPages`, `support/notionGetPage`, `support/notionGetPageBlocks`.
//!
//! Provides read-only access to the Notion REST API.
//! Supports optional block processing controls:
//! - `raw_blocks`: render mode (raw skips Notable lines / Missing info hints).
//! - `max_depth`: limit child block expansion depth (0 disables expansion).

use std::{collections::VecDeque, fmt};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{baml_tool, bundles::Support, tools::BamlTool};
use integrations_notion_read::{
    self as notion_read, NotionReadClient, NotionReadError, RetryAfter,
};
/// Notion REST API base URL.
pub use notion_read::BASE_URL;
/// Notion API version header value.
pub use notion_read::NOTION_VERSION;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
const MAX_BLOCK_DEPTH: u32 = 10;
const BLOCK_TEXT_MAX_CHARS: usize = 200;

fn truncate_chars_with_ellipsis(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars) + 3);
    for (i, ch) in input.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn truncate_optional_text(text: &mut Option<String>, max_chars: usize) {
    let Some(current) = text.as_ref() else {
        return;
    };
    let trimmed = current.trim();
    if trimmed.is_empty() {
        *text = None;
        return;
    }
    if trimmed.chars().count() > max_chars {
        *text = Some(truncate_chars_with_ellipsis(trimmed, max_chars));
    } else if trimmed.len() != current.len() {
        *text = Some(trimmed.to_string());
    }
}
#[cfg(test)]
const RATE_LIMIT_BASE_DELAY_MS: u64 = 500;
#[cfg(test)]
const RATE_LIMIT_MAX_DELAY_MS: u64 = 5_000;
const MAX_BLOCK_PAGES: usize = 10;

mod spans {
    #[inline]
    pub fn notion_search_pages(query_len: Option<usize>, page_size: Option<u32>) -> tracing::Span {
        tracing::debug_span!(
            "baml_tools_notion.notion_search_pages",
            query_len = query_len,
            page_size = page_size
        )
    }

    #[inline]
    pub fn notion_get_page(page_id: &str) -> tracing::Span {
        tracing::debug_span!("baml_tools_notion.notion_get_page", page_id = page_id)
    }

    #[inline]
    pub fn notion_get_page_blocks(block_id: &str) -> tracing::Span {
        tracing::debug_span!(
            "baml_tools_notion.notion_get_page_blocks",
            block_id = block_id
        )
    }

    #[inline]
    pub fn notion_fetch_page_summary(page_id: &str) -> tracing::Span {
        tracing::debug_span!(
            "baml_tools_notion.notion_fetch_page_summary",
            page_id = page_id
        )
    }

    #[inline]
    pub fn notion_fetch_child_blocks(parent_id: &str) -> tracing::Span {
        tracing::debug_span!(
            "baml_tools_notion.notion_fetch_child_blocks",
            parent_id = parent_id
        )
    }
}

#[cfg(test)]
fn backoff_delay(retries: usize) -> std::time::Duration {
    let shift = u32::try_from(retries).unwrap_or(u32::MAX);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let backoff = RATE_LIMIT_BASE_DELAY_MS.saturating_mul(multiplier);
    std::time::Duration::from_millis(backoff.min(RATE_LIMIT_MAX_DELAY_MS))
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

impl baml_rt_tools::DescribeAction for NotionInput {
    fn describe(&self) -> String {
        match self {
            NotionInput::SearchPages(p) => match &p.query {
                Some(q) if !q.is_empty() => format!("searching Notion for '{q}'"),
                _ => "listing all Notion pages".to_string(),
            },
            NotionInput::GetPage(_) => "retrieving Notion page metadata".to_string(),
            NotionInput::GetPageBlocks(_) => "retrieving Notion page content".to_string(),
        }
    }
}

impl baml_rt_tools::DescribeAction for NotionSearchPagesInput {
    fn describe(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("searching Notion for '{q}'"),
            _ => "listing all Notion pages".to_string(),
        }
    }
}

impl baml_rt_tools::DescribeAction for NotionGetPageInput {
    fn describe(&self) -> String {
        "retrieving Notion page metadata".to_string()
    }
}

impl baml_rt_tools::DescribeAction for NotionGetPageBlocksInput {
    fn describe(&self) -> String {
        "retrieving Notion page content".to_string()
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_edited_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct NotionBlockSummary {
    pub id: String,
    pub block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct NotionSource {
    pub page_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotionOperation {
    SearchPages,
    GetPage,
    GetPageBlocks,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct NotionOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<NotionPageSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<NotionBlockSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<NotionSource>,
    pub message: String,
    #[baml(skip)]
    #[schemars(skip)]
    #[ts(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<NotionOperation>,
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

impl From<NotionReadError> for NotionError {
    fn from(value: NotionReadError) -> Self {
        match value {
            NotionReadError::Http(inner) => NotionError::Http(inner),
            NotionReadError::Unauthorized { status, body } => {
                NotionError::Unauthorized { status, body }
            }
            NotionReadError::NotFound { body } => NotionError::NotFound { body },
            NotionReadError::RateLimited { body, retry_after } => {
                NotionError::RateLimited { body, retry_after }
            }
            NotionReadError::Api { status, body } => NotionError::Api { status, body },
            NotionReadError::Deserialize(inner) => NotionError::Deserialize(inner),
            NotionReadError::MissingApiKey => NotionError::MissingApiKey,
            NotionReadError::InvalidId { id } => NotionError::InvalidId { id },
            NotionReadError::InvalidHeader { message } => NotionError::InvalidHeader { message },
            NotionReadError::RequestClone => NotionError::RequestClone,
        }
    }
}

// ---------------------------------------------------------------------------
// Client helpers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct NotionClient {
    client: NotionReadClient,
}

impl NotionClient {
    fn new() -> Self {
        Self {
            client: NotionReadClient::new(),
        }
    }

    #[cfg(test)]
    fn base_url(&self) -> &str {
        self.client.base_url()
    }

    fn api_key(&self) -> std::result::Result<&str, NotionError> {
        self.client.api_key().map_err(NotionError::from)
    }

    fn normalize_id(id: &str) -> std::result::Result<String, NotionError> {
        NotionReadClient::normalize_id(id).map_err(NotionError::from)
    }

    async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, NotionError> {
        self.client
            .send_request(request)
            .await
            .map_err(NotionError::from)
    }

    #[cfg(test)]
    fn parse_retry_after(value: Option<&reqwest::header::HeaderValue>) -> RetryAfter {
        notion_read::parse_retry_after(value)
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
                    .post("/search", api_key)
                    .map_err(NotionError::from)?
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
            operation: Some(NotionOperation::SearchPages),
        })
    }

    async fn get_page(&self, api_key: &str, page_id: &str) -> Result<NotionOutput> {
        let span = spans::notion_get_page(page_id);
        let _guard = span.enter();
        let normalized = Self::normalize_id(page_id)?;
        let json = self
            .send_request(
                self.client
                    .get(&format!("/pages/{normalized}"), api_key)
                    .map_err(NotionError::from)?,
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
            operation: Some(NotionOperation::GetPage),
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
            operation: Some(NotionOperation::GetPageBlocks),
        })
    }

    async fn fetch_page_summary(
        &self,
        api_key: &str,
        page_id: &str,
    ) -> std::result::Result<NotionPageSummary, NotionError> {
        let span = spans::notion_fetch_page_summary(page_id);
        let _guard = span.enter();
        let normalized = Self::normalize_id(page_id)?;
        let json = self
            .send_request(
                self.client
                    .get(&format!("/pages/{normalized}"), api_key)
                    .map_err(NotionError::from)?,
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
        let mut request = self
            .client
            .get(&format!("/blocks/{block_id}/children"), api_key)
            .map_err(NotionError::from)?;
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

#[cfg(test)]
fn should_warn_on_insecure_base_url(base_url: &str) -> bool {
    notion_read::should_warn_on_insecure_base_url(base_url)
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
        should_warn_on_insecure_base_url,
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

    #[test]
    fn notion_insecure_base_url_warning_policy_skips_localhost() {
        assert!(!should_warn_on_insecure_base_url(
            "http://127.0.0.1:8080/v1"
        ));
        assert!(!should_warn_on_insecure_base_url(
            "http://localhost:8080/v1"
        ));
        assert!(should_warn_on_insecure_base_url(
            "http://169.254.169.254/latest"
        ));
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

fn compact_notion_output(output: &mut NotionOutput) {
    if let Some(NotionOperation::GetPageBlocks) = output.operation {
        for block in &mut output.blocks {
            if block.block_type == "notable_lines" {
                continue;
            }
            truncate_optional_text(&mut block.text, BLOCK_TEXT_MAX_CHARS);
        }
    }
    output.operation = None;
}

fn compact_notion_payload(content: &mut serde_json::Value) {
    let Ok(mut output) = serde_json::from_value::<NotionOutput>(content.clone()) else {
        return;
    };
    compact_notion_output(&mut output);
    if let Ok(compacted) = serde_json::to_value(output) {
        *content = compacted;
    }
}

#[baml_tool(
    name = "support/notion",
    description = "Read-only Notion access (search pages, get page, get page blocks).",
    tags = ["support", "notion", "read"],
    access = Read,
    secrets = [
        { name = "NOTION_API_TOKEN", description = "Notion integration token", reason = "Required to authenticate with the Notion API" },
    ],
    baml_types = [
        NotionSearchPagesInput, NotionGetPageInput, NotionGetPageBlocksInput,
        BlockRenderMode, NotionInput, NotionPageSummary, NotionBlockSummary,
        NotionSource, NotionOutput,
    ],
    extra_ts_types = [BlockRenderMode],
)]
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

    fn describe_result(&self, output: &Self::Output) -> String {
        let page_count = output.pages.len();
        let block_count = output.blocks.len();
        if page_count > 0 {
            format!("found {} Notion page(s)", page_count)
        } else if block_count > 0 {
            format!("retrieved {} Notion block(s)", block_count)
        } else {
            "Notion query returned no results".to_string()
        }
    }

    fn describe_open(&self) -> String {
        "using Notion for read-only page retrieval".to_string()
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

#[baml_tool(
    name = "support/notionSearchPages",
    description = "Search Notion pages by query.",
    tags = ["support", "notion", "read"],
    access = Read,
    secrets = [
        { name = "NOTION_API_TOKEN", description = "Notion integration token", reason = "Required to authenticate with the Notion API" },
    ],
    baml_types = [
        NotionSearchPagesInput, NotionPageSummary, NotionBlockSummary,
        NotionSource, NotionOutput,
    ],
)]
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

    fn describe_result(&self, output: &Self::Output) -> String {
        let page_count = output.pages.len();
        let block_count = output.blocks.len();
        if page_count > 0 {
            format!("found {} Notion page(s)", page_count)
        } else if block_count > 0 {
            format!("retrieved {} Notion block(s)", block_count)
        } else {
            "Notion query returned no results".to_string()
        }
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

#[baml_tool(
    name = "support/notionGetPage",
    description = "Fetch a Notion page by id.",
    tags = ["support", "notion", "read"],
    access = Read,
    secrets = [
        { name = "NOTION_API_TOKEN", description = "Notion integration token", reason = "Required to authenticate with the Notion API" },
    ],
    baml_types = [
        NotionGetPageInput, NotionPageSummary, NotionBlockSummary,
        NotionSource, NotionOutput,
    ],
)]
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

    fn describe_result(&self, output: &Self::Output) -> String {
        let page_count = output.pages.len();
        let block_count = output.blocks.len();
        if page_count > 0 {
            format!("found {} Notion page(s)", page_count)
        } else if block_count > 0 {
            format!("retrieved {} Notion block(s)", block_count)
        } else {
            "Notion query returned no results".to_string()
        }
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

#[baml_tool(
    name = "support/notionGetPageBlocks",
    description = "Retrieve Notion block children for a page or block.",
    tags = ["support", "notion", "read"],
    access = Read,
    secrets = [
        { name = "NOTION_API_TOKEN", description = "Notion integration token", reason = "Required to authenticate with the Notion API" },
    ],
    baml_types = [
        BlockRenderMode, NotionGetPageBlocksInput, NotionPageSummary,
        NotionBlockSummary, NotionSource, NotionOutput,
    ],
)]
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

    fn describe_result(&self, output: &Self::Output) -> String {
        let page_count = output.pages.len();
        let block_count = output.blocks.len();
        if page_count > 0 {
            format!("found {} Notion page(s)", page_count)
        } else if block_count > 0 {
            format!("retrieved {} Notion block(s)", block_count)
        } else {
            "Notion query returned no results".to_string()
        }
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

#[cfg(test)]
mod compaction_tests {
    use super::*;

    // -----------------------------------------------------------------------
    // truncate_chars_with_ellipsis
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_below_limit_unchanged() {
        assert_eq!(truncate_chars_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_limit_unchanged() {
        assert_eq!(truncate_chars_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_above_limit_adds_ellipsis() {
        assert_eq!(truncate_chars_with_ellipsis("hello world", 5), "hello...");
    }

    #[test]
    fn truncate_multi_byte_chars() {
        // 3 chars, limit 2 → keeps 2 chars + "..."
        assert_eq!(truncate_chars_with_ellipsis("héllo", 2), "hé...");
    }

    // -----------------------------------------------------------------------
    // truncate_optional_text
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_optional_none_is_noop() {
        let mut val: Option<String> = None;
        truncate_optional_text(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn truncate_optional_empty_becomes_none() {
        let mut val = Some(String::new());
        truncate_optional_text(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn truncate_optional_whitespace_becomes_none() {
        let mut val = Some("   ".to_string());
        truncate_optional_text(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn truncate_optional_within_limit_unchanged() {
        let mut val = Some("short".to_string());
        truncate_optional_text(&mut val, 10);
        assert_eq!(val.as_deref(), Some("short"));
    }

    #[test]
    fn truncate_optional_over_limit_truncates() {
        let mut val = Some("a]".repeat(150));
        truncate_optional_text(&mut val, 10);
        let result = val.unwrap();
        assert!(result.ends_with("..."));
        // 10 chars + "..."
        assert_eq!(result.chars().count(), 13);
    }

    // -----------------------------------------------------------------------
    // compact_notion_output — GetPageBlocks
    // -----------------------------------------------------------------------

    fn make_block(block_type: &str, text: Option<&str>) -> NotionBlockSummary {
        NotionBlockSummary {
            id: "block-1".to_string(),
            block_type: block_type.to_string(),
            text: text.map(|t| t.to_string()),
            has_children: false,
        }
    }

    #[test]
    fn compact_get_page_blocks_truncates_long_text() {
        let long_text = "x".repeat(300);
        let mut output = NotionOutput {
            pages: vec![],
            blocks: vec![make_block("paragraph", Some(&long_text))],
            next_cursor: None,
            has_more: false,
            sources: vec![],
            message: "test".to_string(),
            operation: Some(NotionOperation::GetPageBlocks),
        };
        compact_notion_output(&mut output);
        let text = output.blocks[0].text.as_ref().unwrap();
        assert!(text.ends_with("..."));
        assert!(text.chars().count() <= BLOCK_TEXT_MAX_CHARS + 3);
        assert!(output.operation.is_none());
    }

    #[test]
    fn compact_get_page_blocks_preserves_notable_lines() {
        let long_text = "x".repeat(300);
        let mut output = NotionOutput {
            pages: vec![],
            blocks: vec![make_block("notable_lines", Some(&long_text))],
            next_cursor: None,
            has_more: false,
            sources: vec![],
            message: "test".to_string(),
            operation: Some(NotionOperation::GetPageBlocks),
        };
        compact_notion_output(&mut output);
        let text = output.blocks[0].text.as_ref().unwrap();
        assert_eq!(text.len(), 300);
        assert!(!text.ends_with("..."));
    }

    #[test]
    fn compact_get_page_blocks_none_text_unchanged() {
        let mut output = NotionOutput {
            pages: vec![],
            blocks: vec![make_block("paragraph", None)],
            next_cursor: None,
            has_more: false,
            sources: vec![],
            message: "test".to_string(),
            operation: Some(NotionOperation::GetPageBlocks),
        };
        compact_notion_output(&mut output);
        assert!(output.blocks[0].text.is_none());
    }

    // -----------------------------------------------------------------------
    // compact_notion_output — SearchPages / GetPage (no-op for blocks)
    // -----------------------------------------------------------------------

    #[test]
    fn compact_search_pages_is_noop_for_blocks() {
        let mut output = NotionOutput {
            pages: vec![],
            blocks: vec![],
            next_cursor: None,
            has_more: false,
            sources: vec![],
            message: "test".to_string(),
            operation: Some(NotionOperation::SearchPages),
        };
        compact_notion_output(&mut output);
        assert!(output.operation.is_none());
    }

    #[test]
    fn compact_get_page_is_noop_for_blocks() {
        let mut output = NotionOutput {
            pages: vec![],
            blocks: vec![],
            next_cursor: None,
            has_more: false,
            sources: vec![],
            message: "test".to_string(),
            operation: Some(NotionOperation::GetPage),
        };
        compact_notion_output(&mut output);
        assert!(output.operation.is_none());
    }
}
