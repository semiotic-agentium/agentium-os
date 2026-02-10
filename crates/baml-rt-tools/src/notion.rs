//! Notion tools — `support/notionSearchPages`, `support/notionGetPage`, `support/notionGetPageBlocks`.
//!
//! Provides read-only access to the Notion REST API.

use crate::bundles::Support;
use crate::register_tool_metadata;
use crate::tools::{BamlTool, ToolAccess, ToolFunctionMetadata, ToolSecretRequirement};
use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Notion REST API base URL.
pub const BASE_URL: &str = "https://api.notion.com/v1";
/// Notion API version header value.
pub const NOTION_VERSION: &str = "2025-09-03";

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NotionSearchPagesInput {
    pub query: Option<String>,
    pub start_cursor: Option<String>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NotionGetPageInput {
    pub page_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NotionGetPageBlocksInput {
    pub block_id: String,
    pub start_cursor: Option<String>,
    pub page_size: Option<u32>,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NotionPageSummary {
    pub id: String,
    pub title: String,
    pub url: String,
    pub last_edited_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NotionBlockSummary {
    pub id: String,
    pub block_type: String,
    pub text: Option<String>,
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NotionSource {
    pub page_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NotionOutput {
    pub pages: Vec<NotionPageSummary>,
    pub blocks: Vec<NotionBlockSummary>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub sources: Vec<NotionSource>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum NotionError {
    #[error("Notion HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("Notion API authentication failed ({status}): {body}")]
    Unauthorized { status: u16, body: String },

    #[error("Notion resource not found (404): {body}")]
    NotFound { body: String },

    #[error("Notion rate limit exceeded (429), retry after {retry_after}: {body}")]
    RateLimited { body: String, retry_after: String },

    #[error("Notion API returned {status}: {body}")]
    Api { status: u16, body: String },

    #[error("Failed to deserialize Notion response")]
    Deserialize(#[source] reqwest::Error),

    #[error("Unexpected Notion response shape: {message}")]
    UnexpectedShape { message: String },

    #[error("NOTION_API_TOKEN environment variable not set")]
    MissingApiKey,

    #[error("Invalid Notion id '{id}'")]
    InvalidId { id: String },
}

impl From<NotionError> for BamlRtError {
    fn from(err: NotionError) -> Self {
        match &err {
            NotionError::MissingApiKey | NotionError::Unauthorized { .. } => {
                BamlRtError::Configuration(err.to_string())
            }
            NotionError::InvalidId { .. } | NotionError::NotFound { .. } => {
                BamlRtError::InvalidArgument(err.to_string())
            }
            NotionError::Http(_)
            | NotionError::RateLimited { .. }
            | NotionError::Api { .. }
            | NotionError::Deserialize(_)
            | NotionError::UnexpectedShape { .. } => BamlRtError::ToolExecution(err.to_string()),
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
}

impl NotionClient {
    fn new() -> Self {
        let api_key = std::env::var("NOTION_API_TOKEN").ok();
        Self {
            client: reqwest::Client::new(),
            api_key,
        }
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

    fn auth_headers(&self, api_key: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {api_key}").parse().unwrap(),
        );
        headers.insert("Notion-Version", NOTION_VERSION.parse().unwrap());
        headers
    }

    #[tracing::instrument(skip_all, fields(url))]
    async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, NotionError> {
        let resp = request.send().await.map_err(NotionError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            let code = status.as_u16();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            let body = resp.text().await.unwrap_or_default();
            return Err(match code {
                401 | 403 => NotionError::Unauthorized { status: code, body },
                404 => NotionError::NotFound { body },
                429 => NotionError::RateLimited { body, retry_after },
                _ => NotionError::Api { status: code, body },
            });
        }

        resp.json().await.map_err(NotionError::Deserialize)
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn search_pages(
        &self,
        api_key: &str,
        query: Option<&str>,
        start_cursor: Option<&str>,
        page_size: Option<u32>,
    ) -> Result<NotionOutput> {
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
                    .post(format!("{BASE_URL}/search"))
                    .headers(self.auth_headers(api_key))
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

    #[tracing::instrument(skip(self, api_key))]
    async fn get_page(&self, api_key: &str, page_id: &str) -> Result<NotionOutput> {
        let normalized = Self::normalize_id(page_id)?;
        let json = self
            .send_request(
                self.client
                    .get(format!("{BASE_URL}/pages/{normalized}"))
                    .headers(self.auth_headers(api_key)),
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

    #[tracing::instrument(skip(self, api_key))]
    async fn get_page_blocks(
        &self,
        api_key: &str,
        block_id: &str,
        start_cursor: Option<&str>,
        page_size: Option<u32>,
    ) -> Result<NotionOutput> {
        let normalized = Self::normalize_id(block_id)?;
        let mut request = self
            .client
            .get(format!("{BASE_URL}/blocks/{normalized}/children"))
            .headers(self.auth_headers(api_key));
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

        let blocks = extract_blocks(&json);
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
        if let Some(parent_page_id) = extract_parent_page_id(&json)
            && let Ok(page) = self.fetch_page_summary(api_key, &parent_page_id).await
        {
            sources.push(NotionSource {
                page_id: page.id.clone(),
                url: page.url.clone(),
            });
            pages.push(page);
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

    #[tracing::instrument(skip(self, api_key))]
    async fn fetch_page_summary(
        &self,
        api_key: &str,
        page_id: &str,
    ) -> std::result::Result<NotionPageSummary, NotionError> {
        let normalized = Self::normalize_id(page_id)?;
        let json = self
            .send_request(
                self.client
                    .get(format!("{BASE_URL}/pages/{normalized}"))
                    .headers(self.auth_headers(api_key)),
            )
            .await?;
        parse_page_summary(&json).ok_or_else(|| NotionError::UnexpectedShape {
            message: "unexpected page shape".to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

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
        self.client
            .get_page_blocks(
                api_key,
                &args.block_id,
                args.start_cursor.as_deref(),
                args.page_size,
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

fn extract_blocks(json: &serde_json::Value) -> Vec<NotionBlockSummary> {
    let Some(results) = json.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    results.iter().filter_map(parse_block_summary).collect()
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
    let rich_text = block.get("rich_text")?.as_array()?;
    let text = rich_text
        .iter()
        .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() { None } else { Some(text) }
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
    TypeBasedMetadataBuilder::<(), NotionSearchPagesInput, NotionOutput>::new(
        name,
        class_name,
        "Search Notion pages by query.".to_string(),
    )
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
    TypeBasedMetadataBuilder::<(), NotionGetPageInput, NotionOutput>::new(
        name,
        class_name,
        "Fetch a Notion page by id.".to_string(),
    )
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
    TypeBasedMetadataBuilder::<(), NotionGetPageBlocksInput, NotionOutput>::new(
        name,
        class_name,
        "Retrieve Notion block children for a page or block.".to_string(),
    )
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
