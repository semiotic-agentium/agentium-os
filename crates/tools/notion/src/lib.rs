// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Notion tools — `support/notionSearchPages`, `support/notionGetPage`, `support/notionGetPageBlocks`.
//!
//! Provides read-only access to the Notion REST API.
//! Supports optional block processing controls:
//! - `raw_blocks`: render mode (raw skips Notable lines / Missing info hints).
//! - `max_depth`: limit child block expansion depth (0 disables expansion).

use std::{collections::VecDeque, fmt, sync::Arc};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result, retry_after::RetryAfter, semantics::ErrorDisposition};
use baml_rt_tools::{
    ClassifiedToolError, baml_tool,
    bundles::Support,
    tools::{BamlTool, ToolHandler, create_tool_handler},
};
use integrations_notion_read::{self as notion_read, NotionReadClient, NotionReadError};
/// Notion REST API base URL.
pub use notion_read::BASE_URL;
/// Notion API version header value.
pub use notion_read::NOTION_VERSION;
use serde::{Deserialize, Serialize};
use tracing::Instrument;
const MAX_BLOCK_DEPTH: u32 = 10;

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

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// Search Notion pages by keyword. Returns a list of page titles and IDs.
/// Does NOT return page content — use GetPageBlocks to read actual content.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct NotionSearchPagesInput {
    #[baml(description = "Search keyword. Omit to list all pages.")]
    pub query: Option<String>,
    #[baml(description = "Pagination cursor from a previous search result.")]
    pub start_cursor: Option<String>,
    #[baml(
        description = "Max pages to return per call (default 100). Use start_cursor to paginate."
    )]
    pub page_size: Option<u32>,
}

/// Fetch page METADATA only: title, URL, last-edited timestamp.
/// Returns NO page content (text, headings, bullets, etc.).
/// To read the actual content of a page, use GetPageBlocks with the same ID.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct NotionGetPageInput {
    #[baml(
        description = "Notion page UUID from a SearchPages result, e.g. '238cff78-8181-80c8-8273-cc5fbdd8c7da'."
    )]
    pub page_id: String,
}

/// Fetch the actual TEXT CONTENT of a Notion page as structured blocks
/// (headings, paragraphs, bullet lists, etc.).
/// block_id is the same UUID as the page id returned by SearchPages.
/// Use this — not GetPage — when you need to read what a page says.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct NotionGetPageBlocksInput {
    #[baml(
        description = "Page UUID to read blocks from — same value as the page id from SearchPages."
    )]
    pub block_id: String,
    #[baml(description = "Pagination cursor for large pages.")]
    pub start_cursor: Option<String>,
    pub page_size: Option<u32>,
    #[baml(description = "Block render mode (omit for default enriched rendering).")]
    pub raw_blocks: Option<BlockRenderMode>,
    #[baml(description = "Max child block depth to expand (omit to expand all children).")]
    pub max_depth: Option<u32>,
}

/// Notion tool input — three mutually exclusive operations:
/// SearchPages: find pages by keyword (returns titles+IDs, no content).
/// GetPage: fetch page metadata only (title, URL, timestamps — NOT content).
/// GetPageBlocks: read the actual text content of a page. Use this to understand what a page says.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[baml(union)]
#[serde(untagged)]
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

baml_rt_tools::impl_describe_action_identity! {
    for NotionInput {
        SearchPages(p) => "search_pages" { query: p.query },
        GetPage(p) => "get_page" { page_id: p.page_id },
        GetPageBlocks(p) => "get_page_blocks" { block_id: p.block_id },
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

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct NotionPageSummary {
    pub id: String,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_edited_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct NotionBlockSummary {
    pub id: String,
    pub block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
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

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<NotionOperation>,
}

/// Controls how Notion blocks are rendered in the output.
/// Enriched (default): adds Notable Lines summaries and Missing Info hints for tables.
/// Raw: returns blocks verbatim without post-processing or hints.
#[derive(Debug, Default, Clone, Copy, Serialize, BamlType)]
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
    fn new() -> std::result::Result<Self, NotionError> {
        Ok(Self {
            client: NotionReadClient::new().map_err(NotionError::from)?,
        })
    }

    #[cfg(test)]
    fn base_url(&self) -> &str {
        self.client.base_url()
    }

    fn api_key(&self) -> &str {
        self.client.api_key()
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

    async fn search_pages(
        &self,
        api_key: &str,
        query: Option<&str>,
        start_cursor: Option<&str>,
        page_size: Option<u32>,
    ) -> Result<NotionOutput> {
        async {
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
        .instrument(spans::notion_search_pages(
            query.map(|q| q.len()),
            page_size,
        ))
        .await
    }

    async fn get_page(&self, api_key: &str, page_id: &str) -> Result<NotionOutput> {
        async {
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

            let page_id_for_blocks = pages
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| normalized.to_string());
            Ok(NotionOutput {
                pages,
                blocks: Vec::new(),
                next_cursor: None,
                has_more: false,
                sources,
                message: format!(
                    "Page metadata only: title and URL for '{title}'. \
                     This result contains NO page content. \
                     To read the actual page text, use GetPageBlocks with block_id: {page_id_for_blocks}"
                ),
                operation: Some(NotionOperation::GetPage),
            })
        }
        .instrument(spans::notion_get_page(page_id))
        .await
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
        async {
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
        .instrument(spans::notion_get_page_blocks(block_id))
        .await
    }

    async fn fetch_page_summary(
        &self,
        api_key: &str,
        page_id: &str,
    ) -> std::result::Result<NotionPageSummary, NotionError> {
        async {
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
        .instrument(spans::notion_fetch_page_summary(page_id))
        .await
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
        async {
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
        .instrument(spans::notion_get_page_blocks(block_id))
        .await
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
            let (_pages, _sources, child_blocks, _next, _has_more) = self
                .fetch_blocks_all_pages(api_key, &block_id, None, None, render_mode, false)
                .instrument(spans::notion_fetch_child_blocks(&block_id))
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

    use test_support::common::TempEnvVar;

    use super::{
        BASE_URL, NotionClient, NotionInput, next_depth_for_children,
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
        use baml_rt_core::backoff::backoff_delay;
        let base = Duration::from_millis(500);
        let max = Duration::from_millis(5_000);
        assert_eq!(backoff_delay(base, max, 0), Duration::from_millis(500));
        assert_eq!(backoff_delay(base, max, 1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(base, max, 2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(base, max, 3), Duration::from_millis(4000));
        assert_eq!(backoff_delay(base, max, 4), Duration::from_millis(5000));
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
        let _token = TempEnvVar::set("NOTION_API_TOKEN", "test-token");
        let client = NotionClient::new().expect("construct notion client");
        assert_eq!(client.base_url(), BASE_URL);
    }

    #[test]
    fn notion_base_url_uses_override_and_trims_trailing_slash() {
        let _guard = notion_api_base_url_test_lock()
            .lock()
            .expect("lock notion base URL test mutex");
        let _env = TempEnvVar::set("NOTION_API_BASE_URL", " https://mock.notion.local/v1/ ");
        let _token = TempEnvVar::set("NOTION_API_TOKEN", "test-token");
        let client = NotionClient::new().expect("construct notion client");
        assert_eq!(client.base_url(), "https://mock.notion.local/v1");
    }

    #[test]
    fn notion_base_url_is_bound_at_client_creation() {
        let _guard = notion_api_base_url_test_lock()
            .lock()
            .expect("lock notion base URL test mutex");
        let _env_unset = TempEnvVar::remove("NOTION_API_BASE_URL");
        let _token = TempEnvVar::set("NOTION_API_TOKEN", "test-token");
        let client = NotionClient::new().expect("construct notion client");
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

impl NotionTool {
    pub fn new() -> std::result::Result<Self, NotionError> {
        Ok(Self {
            client: NotionClient::new()?,
        })
    }
}

fn build_notion_tool() -> Result<Arc<dyn ToolHandler>> {
    let tool = NotionTool::new()?;
    create_tool_handler(tool).map(|(_, h)| h)
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
    build_with = build_notion_tool,
)]
#[async_trait]
impl BamlTool for NotionTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "notion";
    const SESSION_POLICY: baml_rt_tools::SessionPolicy = baml_rt_tools::SessionPolicy::MultiSend;
    type OpenInput = ();
    type Input = NotionInput;
    type Output = NotionOutput;

    fn description(&self) -> &'static str {
        "Read-only Notion access: search pages, get page, and retrieve page blocks."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = self.client.api_key();
        let mut output = match args {
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
        }?;
        output.operation = None;
        Ok(output)
    }

    fn action_identity(&self, input: &Self::Input) -> Option<baml_rt_tools::ActionIdentity> {
        Some(baml_rt_tools::DescribeActionIdentity::action_identity(
            input,
        ))
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

    fn classify_execution_error(err: &BamlRtError) -> ClassifiedToolError {
        let mut c = ClassifiedToolError::from_baml_error(err);
        if let BamlRtError::ToolExecution(msg) = err {
            let lower = msg.to_ascii_lowercase();
            if lower.contains("object_not_found") || lower.contains("could not find") {
                c.disposition = ErrorDisposition::InformAndContinue;
                c.code = "notion_not_found".to_string();
                c.hint = Some(
                    "Confirm page_id / block_id and that the integration has access to the page."
                        .to_string(),
                );
            } else if lower.contains("rate_limited")
                || lower.contains("too many requests")
                || lower.contains("429")
            {
                c.disposition = ErrorDisposition::HostRetriable;
                c.code = "notion_rate_limited".to_string();
            }
        }
        c
    }
}

pub struct NotionSearchPagesTool {
    client: NotionClient,
}

impl NotionSearchPagesTool {
    pub fn new() -> std::result::Result<Self, NotionError> {
        Ok(Self {
            client: NotionClient::new()?,
        })
    }
}

fn build_notion_search_pages_tool() -> Result<Arc<dyn ToolHandler>> {
    let tool = NotionSearchPagesTool::new()?;
    create_tool_handler(tool).map(|(_, h)| h)
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
    build_with = build_notion_search_pages_tool,
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
        let api_key = self.client.api_key();
        let mut output = self
            .client
            .search_pages(
                api_key,
                args.query.as_deref(),
                args.start_cursor.as_deref(),
                args.page_size,
            )
            .await?;
        output.operation = None;
        Ok(output)
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

impl NotionGetPageTool {
    pub fn new() -> std::result::Result<Self, NotionError> {
        Ok(Self {
            client: NotionClient::new()?,
        })
    }
}

fn build_notion_get_page_tool() -> Result<Arc<dyn ToolHandler>> {
    let tool = NotionGetPageTool::new()?;
    create_tool_handler(tool).map(|(_, h)| h)
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
    build_with = build_notion_get_page_tool,
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
        let api_key = self.client.api_key();
        let mut output = self.client.get_page(api_key, &args.page_id).await?;
        output.operation = None;
        Ok(output)
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

impl NotionGetPageBlocksTool {
    pub fn new() -> std::result::Result<Self, NotionError> {
        Ok(Self {
            client: NotionClient::new()?,
        })
    }
}

fn build_notion_get_page_blocks_tool() -> Result<Arc<dyn ToolHandler>> {
    let tool = NotionGetPageBlocksTool::new()?;
    create_tool_handler(tool).map(|(_, h)| h)
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
    build_with = build_notion_get_page_blocks_tool,
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
        let api_key = self.client.api_key();
        let render_mode = args.raw_blocks.unwrap_or_default();
        let max_depth = args.max_depth.unwrap_or(2).min(MAX_BLOCK_DEPTH);
        let mut output = self
            .client
            .get_page_blocks(
                api_key,
                &args.block_id,
                args.start_cursor.as_deref(),
                args.page_size,
                render_mode,
                max_depth,
            )
            .await?;
        output.operation = None;
        Ok(output)
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
