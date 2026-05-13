//! Conversation-history service contract and API DTOs.
//!
//! Design note (read-envelope choice): we intentionally use a route-scoped DTO
//! (`ConversationHistoryPageDto`) and reuse existing `EpisodeSnapshotDto` for episode
//! routes. Shared pagination semantics are encoded through `CursorToken` and
//! `ConversationHistoryPageRequest` rather than a cross-route `Option` bag.

use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    error::Error,
    fmt,
    hash::{Hash, Hasher},
};

use async_trait::async_trait;
use baml_rt_conversation::view::{
    ConversationItemContent, ProvenanceConversationContextItem, ToolOutcome, ToolSessionPhase,
};
use baml_rt_core::{
    Citation,
    bus::SessionStepOp,
    ids::{ContextId, ExternalId, TaskId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

fn is_false(b: &bool) -> bool {
    !*b
}

/// Default page size when the client omits `limit` on conversation-history routes.
pub const DEFAULT_CONVERSATION_HISTORY_LIMIT: u32 = 50;

/// Service errors for conversation-history reads.
pub type ConversationHistoryError = crate::service_error::ServiceError;

pub use baml_rt_core::ConversationHistoryUpdate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationHistoryProfile {
    Full,
    Compact,
}

impl ConversationHistoryProfile {
    fn parse(raw: Option<&str>) -> Result<Self, ConversationHistoryRequestParseError> {
        match raw.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(Self::Full),
            Some(v) if v.eq_ignore_ascii_case("full") => Ok(Self::Full),
            Some(v) if v.eq_ignore_ascii_case("compact") => Ok(Self::Compact),
            Some(other) => Err(ConversationHistoryRequestParseError::InvalidProfile(
                other.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationHistoryFormat {
    Full,
}

impl ConversationHistoryFormat {
    fn parse(raw: Option<&str>) -> Result<Self, ConversationHistoryRequestParseError> {
        match raw.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(Self::Full),
            Some(v) if v.eq_ignore_ascii_case("full") => Ok(Self::Full),
            Some(other) => Err(ConversationHistoryRequestParseError::UnsupportedFormat(
                other.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorToken(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CursorStateV1 {
    v: u8,
    offset: usize,
    context_id: String,
    task_id: Option<String>,
}

impl CursorToken {
    pub fn encode_v1(offset: usize, context_id: &ContextId, task_id: Option<&TaskId>) -> Self {
        let state = CursorStateV1 {
            v: 1,
            offset,
            context_id: context_id.as_str().to_string(),
            task_id: task_id.map(|id| id.as_str().to_string()),
        };
        let bytes = serde_json::to_vec(&state).expect("cursor state v1 serializes");
        Self(format!("v1.{:x}", HexBytes(bytes)))
    }

    fn decode_v1(&self) -> Result<CursorStateV1, ConversationHistoryRequestParseError> {
        let payload = self.0.strip_prefix("v1.").ok_or_else(|| {
            ConversationHistoryRequestParseError::InvalidCursor("missing v1 prefix".to_string())
        })?;
        let bytes = decode_hex(payload)
            .map_err(|e| ConversationHistoryRequestParseError::InvalidCursor(e.to_string()))?;
        let state: CursorStateV1 = serde_json::from_slice(&bytes)
            .map_err(|e| ConversationHistoryRequestParseError::InvalidCursor(e.to_string()))?;
        if state.v != 1 {
            return Err(ConversationHistoryRequestParseError::UnknownCursorVersion(
                state.v as u32,
            ));
        }
        Ok(state)
    }
}

struct HexBytes(Vec<u8>);
impl fmt::LowerHex for HexBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

fn decode_hex(input: &str) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    if !input.len().is_multiple_of(2) {
        return Err("hex payload must have even length".into());
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let hi = (bytes[i] as char)
            .to_digit(16)
            .ok_or_else(|| "invalid hex digit".to_string())?;
        let lo = (bytes[i + 1] as char)
            .to_digit(16)
            .ok_or_else(|| "invalid hex digit".to_string())?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub enum ConversationHistoryPageRequest {
    First { limit: usize },
    Next { limit: usize, cursor: CursorToken },
}

impl ConversationHistoryPageRequest {
    pub fn limit(&self) -> usize {
        match self {
            Self::First { limit } | Self::Next { limit, .. } => *limit,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConversationHistoryRequest {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub page: ConversationHistoryPageRequest,
    pub profile: ConversationHistoryProfile,
    pub format: ConversationHistoryFormat,
}

#[derive(Debug, Clone)]
pub struct ConversationHistoryDeltaRequest {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub after_event_order: u64,
    pub limit: usize,
    pub profile: ConversationHistoryProfile,
    pub format: ConversationHistoryFormat,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHistoryQueryParams {
    pub task_id: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub profile: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug)]
pub enum ConversationHistoryRequestParseError {
    MissingContextId,
    InvalidLimit(u32),
    InvalidTaskId,
    InvalidProfile(String),
    UnsupportedFormat(String),
    InvalidCursor(String),
    UnknownCursorVersion(u32),
    CursorScopeMismatch,
}

impl fmt::Display for ConversationHistoryRequestParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContextId => write!(f, "context_id is required"),
            Self::InvalidLimit(v) => write!(f, "limit must be in range [1, 500], got {v}"),
            Self::InvalidTaskId => write!(f, "taskId must not be empty"),
            Self::InvalidProfile(p) => write!(f, "profile must be one of: full, compact (got {p})"),
            Self::UnsupportedFormat(v) => write!(f, "unsupported format '{v}'"),
            Self::InvalidCursor(e) => write!(f, "invalid cursor: {e}"),
            Self::UnknownCursorVersion(v) => write!(f, "unknown cursor version {v}"),
            Self::CursorScopeMismatch => {
                write!(
                    f,
                    "cursor does not match contextId/taskId scope for this request"
                )
            }
        }
    }
}

impl Error for ConversationHistoryRequestParseError {}

impl ConversationHistoryRequest {
    pub fn from_parts(
        context_id_raw: &str,
        params: ConversationHistoryQueryParams,
    ) -> Result<Self, ConversationHistoryRequestParseError> {
        let context_raw = context_id_raw.trim();
        if context_raw.is_empty() {
            return Err(ConversationHistoryRequestParseError::MissingContextId);
        }
        let context_id = ContextId::from(context_raw);
        let task_id = match params.task_id.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(TaskId::from_external(ExternalId::new(raw.to_string()))),
        };
        if params.task_id.as_deref() == Some("") {
            return Err(ConversationHistoryRequestParseError::InvalidTaskId);
        }

        let raw_limit = params.limit.unwrap_or(DEFAULT_CONVERSATION_HISTORY_LIMIT);
        if !(1..=500).contains(&raw_limit) {
            return Err(ConversationHistoryRequestParseError::InvalidLimit(
                raw_limit,
            ));
        }
        let limit = raw_limit as usize;
        let profile = ConversationHistoryProfile::parse(params.profile.as_deref())?;
        let format = ConversationHistoryFormat::parse(params.format.as_deref())?;

        let page = match params.cursor.map(CursorToken) {
            None => ConversationHistoryPageRequest::First { limit },
            Some(cursor) => {
                let state = cursor.decode_v1()?;
                if state.context_id != context_id.as_str()
                    || state.task_id.as_deref() != task_id.as_ref().map(TaskId::as_str)
                {
                    return Err(ConversationHistoryRequestParseError::CursorScopeMismatch);
                }
                ConversationHistoryPageRequest::Next { limit, cursor }
            }
        };

        Ok(Self {
            context_id,
            task_id,
            page,
            profile,
            format,
        })
    }

    pub fn offset_from_cursor(&self) -> Result<usize, ConversationHistoryRequestParseError> {
        match &self.page {
            ConversationHistoryPageRequest::First { .. } => Ok(0),
            ConversationHistoryPageRequest::Next { cursor, .. } => Ok(cursor.decode_v1()?.offset),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHistoryPageDto {
    pub context_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub version: String,
    pub max_event_order: u64,
    pub items: Vec<ConversationHistoryItemDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Latest LLM prompt JSON UTF-8 byte length in this scope (temporal tail).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_context_bytes_session_current: Option<u64>,
    /// Latest LLM prompt message character count in this scope (same tail as bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_message_chars_session_current: Option<u64>,
    /// LLM prompt operations through `max_event_order` (delta may contain only new rows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub llm_prompt_operations: Vec<LlmPromptOperationDto>,
    /// True when the effective task's most recent
    /// [`baml_rt_provenance::metamodel::A2ATaskStateProps`] carries
    /// `TaskStatusKind::InputRequired`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub awaiting_input: bool,
    /// Prompt body from the latest `InputRequired` state. Clients must
    /// treat absence as unknown rather than as a signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_required_prompt: Option<String>,
}

/// One completed LLM call’s prompt telemetry (for UI / SSE merge).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LlmPromptOperationDto {
    pub activity_anchor: String,
    pub event_order: u64,
    /// UTF-8 length of JSON-serialized prompt payload.
    pub prompt_context_bytes_current: u64,
    /// Unicode scalar count of chat message text in the request.
    pub prompt_message_chars_current: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHistoryItemDto {
    pub timestamp_ms: u64,
    pub activity_anchor: String,
    pub role: String,
    pub content: ConversationHistoryContentDto,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationHistoryContentDto {
    Message {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<String>,
    },
    ToolCall {
        tool_name: String,
        args: Value,
        fsm_phase: String,
    },
    ToolResult {
        tool_name: String,
        fsm_phase: String,
        outcome: ToolOutcomeDto,
    },
    SessionStep {
        tool_name: String,
        op: SessionStepOpDto,
        #[serde(skip_serializing_if = "Option::is_none")]
        send_done_replay_payload: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        read_replay_lines: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolOutcomeDto {
    Result { value: Value },
    Error { value: Value },
    StatusOnly,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionStepOpDto {
    Open,
    SendDone {
        archive_ref: String,
        header: String,
        informed_by: String,
    },
    SearchRead {
        archive_ref: String,
        grep: String,
        offset: usize,
        limit: usize,
    },
    PageRead {
        archive_ref: String,
        offset: usize,
        limit: usize,
    },
}

fn session_phase_to_string(value: ToolSessionPhase) -> String {
    match value {
        ToolSessionPhase::Execute => "execute".to_string(),
        ToolSessionPhase::Open => "open".to_string(),
        ToolSessionPhase::Send => "send".to_string(),
        ToolSessionPhase::Read => "read".to_string(),
        ToolSessionPhase::Next => "next".to_string(),
        ToolSessionPhase::Finish => "finish".to_string(),
        ToolSessionPhase::Abort => "abort".to_string(),
        ToolSessionPhase::Unknown(v) => v,
    }
}

impl From<SessionStepOp> for SessionStepOpDto {
    fn from(value: SessionStepOp) -> Self {
        match value {
            SessionStepOp::Open => Self::Open,
            SessionStepOp::SendDone {
                archive_ref,
                header,
                informed_by,
            } => Self::SendDone {
                archive_ref,
                header,
                informed_by,
            },
            SessionStepOp::SearchRead {
                archive_ref,
                grep,
                offset,
                limit,
            } => Self::SearchRead {
                archive_ref,
                grep,
                offset,
                limit,
            },
            SessionStepOp::PageRead {
                archive_ref,
                offset,
                limit,
            } => Self::PageRead {
                archive_ref,
                offset,
                limit,
            },
        }
    }
}

impl From<ToolOutcome> for ToolOutcomeDto {
    fn from(value: ToolOutcome) -> Self {
        match value {
            ToolOutcome::Result(value) => Self::Result { value },
            ToolOutcome::Error(value) => Self::Error { value },
            ToolOutcome::StatusOnly => Self::StatusOnly,
        }
    }
}

impl From<ConversationItemContent> for ConversationHistoryContentDto {
    fn from(value: ConversationItemContent) -> Self {
        match value {
            ConversationItemContent::Message { text, citations } => Self::Message {
                text,
                citations: citations
                    .into_iter()
                    .map(|c: Citation| c.to_string())
                    .collect(),
            },
            ConversationItemContent::ToolCall(tool_call) => Self::ToolCall {
                tool_name: tool_call.tool_name,
                args: tool_call.args,
                fsm_phase: session_phase_to_string(tool_call.fsm_phase),
            },
            ConversationItemContent::ToolResult(tool_result) => Self::ToolResult {
                tool_name: tool_result.tool_name,
                fsm_phase: session_phase_to_string(tool_result.fsm_phase),
                outcome: tool_result.outcome.into(),
            },
            ConversationItemContent::SessionStep(session_step) => Self::SessionStep {
                tool_name: session_step.tool_name,
                op: session_step.op.into(),
                send_done_replay_payload: session_step.send_done_replay_payload,
                read_replay_lines: session_step.read_replay_lines,
            },
        }
    }
}

impl From<ProvenanceConversationContextItem> for ConversationHistoryItemDto {
    fn from(value: ProvenanceConversationContextItem) -> Self {
        Self {
            timestamp_ms: value.timestamp_ms,
            activity_anchor: value.activity_anchor.as_str().to_string(),
            role: value.role,
            content: value.content.into(),
        }
    }
}

/// Loads every page for `request` (same `limit` per chunk), merges items and prompt-operation rows,
/// clears `next_cursor`, and recomputes [`page_version`] for the merged transcript.
pub async fn merge_conversation_history_pages(
    svc: &dyn ConversationHistoryService,
    request: &ConversationHistoryRequest,
) -> Result<ConversationHistoryPageDto, ConversationHistoryError> {
    let limit = request.page.limit();
    let mut merged_items: Vec<ConversationHistoryItemDto> = Vec::new();
    let mut merged_ops: Vec<LlmPromptOperationDto> = Vec::new();
    let mut seen_ops: HashSet<(String, u64)> = HashSet::new();
    let mut max_event_order: u64 = 0;
    let mut req = request.clone();

    let mut page = svc.page(&req).await?;
    loop {
        max_event_order = max_event_order.max(page.max_event_order);
        for op in &page.llm_prompt_operations {
            let key = (op.activity_anchor.clone(), op.event_order);
            if seen_ops.insert(key) {
                merged_ops.push(op.clone());
            }
        }
        merged_items.append(&mut page.items);

        let Some(cursor) = page.next_cursor.take() else {
            page.items = merged_items;
            merged_ops.sort_by_key(|op| op.event_order);
            page.llm_prompt_operations = merged_ops;
            page.next_cursor = None;
            page.max_event_order = max_event_order.max(
                page.items
                    .iter()
                    .map(|item| item.timestamp_ms)
                    .max()
                    .unwrap_or(0),
            );
            page.version = page_version(
                &page.items,
                &page.llm_prompt_operations,
                page.prompt_context_bytes_session_current,
                page.prompt_message_chars_session_current,
                page.awaiting_input,
                page.input_required_prompt.as_deref(),
            );
            return Ok(page);
        };

        req = ConversationHistoryRequest {
            context_id: request.context_id.clone(),
            task_id: request.task_id.clone(),
            page: ConversationHistoryPageRequest::Next {
                limit,
                cursor: CursorToken(cursor),
            },
            profile: request.profile,
            format: request.format,
        };
        page = svc.page(&req).await?;
    }
}

#[async_trait]
pub trait ConversationHistoryService: Send + Sync {
    async fn page(
        &self,
        request: &ConversationHistoryRequest,
    ) -> Result<ConversationHistoryPageDto, ConversationHistoryError>;

    async fn delta_after_event_order(
        &self,
        request: &ConversationHistoryDeltaRequest,
    ) -> Result<ConversationHistoryPageDto, ConversationHistoryError>;
}

pub fn page_version(
    items: &[ConversationHistoryItemDto],
    llm_prompt_operations: &[LlmPromptOperationDto],
    prompt_context_bytes_session_current: Option<u64>,
    prompt_message_chars_session_current: Option<u64>,
    awaiting_input: bool,
    input_required_prompt: Option<&str>,
) -> String {
    let mut hasher = DefaultHasher::new();
    for item in items {
        item.timestamp_ms.hash(&mut hasher);
        item.activity_anchor.hash(&mut hasher);
        item.role.hash(&mut hasher);
        let content = serde_json::to_string(&item.content).unwrap_or_default();
        content.hash(&mut hasher);
    }
    for op in llm_prompt_operations {
        op.activity_anchor.hash(&mut hasher);
        op.event_order.hash(&mut hasher);
        op.prompt_context_bytes_current.hash(&mut hasher);
        op.prompt_message_chars_current.hash(&mut hasher);
    }
    prompt_context_bytes_session_current.hash(&mut hasher);
    prompt_message_chars_session_current.hash(&mut hasher);
    awaiting_input.hash(&mut hasher);
    input_required_prompt.hash(&mut hasher);
    format!("v1:{:x}", hasher.finish())
}

pub trait ConversationHistoryEventService: Send + Sync {
    fn subscribe_updates(&self) -> tokio::sync::broadcast::Receiver<ConversationHistoryUpdate>;
}

pub fn paginate_items(
    mut rows: Vec<ProvenanceConversationContextItem>,
    request: &ConversationHistoryRequest,
) -> Result<ConversationHistoryPageDto, ConversationHistoryError> {
    rows.sort_by(|a, b| {
        a.timestamp_ms
            .cmp(&b.timestamp_ms)
            .then_with(|| a.activity_anchor.as_str().cmp(b.activity_anchor.as_str()))
    });

    let start = request
        .offset_from_cursor()
        .map_err(|e| ConversationHistoryError::Other(Box::new(e)))?;
    if start > rows.len() {
        return Err(ConversationHistoryError::NotFound);
    }
    let limit = request.page.limit();
    let end = start.saturating_add(limit).min(rows.len());
    let page_rows = rows[start..end].to_vec();
    let next_cursor = if end < rows.len() {
        Some(
            CursorToken::encode_v1(end, &request.context_id, request.task_id.as_ref())
                .0
                .to_string(),
        )
    } else {
        None
    };
    let items: Vec<ConversationHistoryItemDto> = page_rows.into_iter().map(Into::into).collect();
    let max_event_order = items.last().map(|item| item.timestamp_ms).unwrap_or(0);
    let version = page_version(&items, &[], None, None, false, None);

    Ok(ConversationHistoryPageDto {
        context_id: request.context_id.as_str().to_string(),
        task_id: request.task_id.as_ref().map(|id| id.as_str().to_string()),
        version,
        max_event_order,
        items,
        next_cursor,
        prompt_context_bytes_session_current: None,
        prompt_message_chars_session_current: None,
        llm_prompt_operations: Vec::new(),
        awaiting_input: false,
        input_required_prompt: None,
    })
}

pub fn profile_filter(
    item: ConversationHistoryItemDto,
    profile: ConversationHistoryProfile,
) -> ConversationHistoryItemDto {
    match profile {
        ConversationHistoryProfile::Full => item,
        ConversationHistoryProfile::Compact => {
            let compact_content = match item.content {
                ConversationHistoryContentDto::ToolResult {
                    tool_name,
                    fsm_phase,
                    outcome,
                } => {
                    let outcome = match outcome {
                        ToolOutcomeDto::Result { value } => ToolOutcomeDto::Result {
                            value: compact_json(value),
                        },
                        ToolOutcomeDto::Error { value } => ToolOutcomeDto::Error {
                            value: compact_json(value),
                        },
                        ToolOutcomeDto::StatusOnly => ToolOutcomeDto::StatusOnly,
                    };
                    ConversationHistoryContentDto::ToolResult {
                        tool_name,
                        fsm_phase,
                        outcome,
                    }
                }
                ConversationHistoryContentDto::ToolCall {
                    tool_name,
                    args,
                    fsm_phase,
                } => ConversationHistoryContentDto::ToolCall {
                    tool_name,
                    args: compact_json(args),
                    fsm_phase,
                },
                other => other,
            };
            ConversationHistoryItemDto {
                content: compact_content,
                ..item
            }
        }
    }
}

fn compact_json(value: Value) -> Value {
    const MAX_STRING_LEN: usize = 512;
    match value {
        Value::String(s) if s.chars().count() > MAX_STRING_LEN => {
            let compact = s.chars().take(MAX_STRING_LEN).collect::<String>();
            Value::String(format!("{compact}…"))
        }
        Value::Array(mut arr) if arr.len() > 64 => {
            arr.truncate(64);
            Value::Array(arr)
        }
        Value::Object(mut map) if map.len() > 64 => {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for k in keys.iter().skip(64) {
                map.remove(k);
            }
            Value::Object(map)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationHistoryQueryParams, ConversationHistoryRequest,
        DEFAULT_CONVERSATION_HISTORY_LIMIT,
    };

    #[test]
    fn conversation_history_default_limit_matches_constant() {
        let req = ConversationHistoryRequest::from_parts(
            "ctx-scope-test",
            ConversationHistoryQueryParams {
                task_id: None,
                limit: None,
                cursor: None,
                profile: None,
                format: None,
            },
        )
        .expect("valid request");
        assert_eq!(
            req.page.limit(),
            DEFAULT_CONVERSATION_HISTORY_LIMIT as usize
        );
    }
}
