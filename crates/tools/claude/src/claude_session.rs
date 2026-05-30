// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! claude/dev tool: host-owned Claude session orchestration.

// `ToolSessionError` carries rich context; `-D clippy::result_large_err` is not worth API churn here.
#![expect(
    clippy::result_large_err,
    reason = "ToolSessionError is large by design; boxing would churn every session-tool signature"
)]

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    BundleName, ToolBundle, ToolBundleMetadata, ToolCapability, ToolFailure, ToolHandler,
    ToolSession, ToolSessionError, ToolStep,
    bundles::BundleType,
    opaque_json_map_from_object,
    tools::{
        HistoryContextSessionOp, HistoryContextStatus, HistoryContextV1, ToolFunctionMetadata,
        ToolProjectionSemantics, ToolSessionContext, validate_open_input,
    },
};
use claude_agent_sdk_rs::{
    ClaudeAgentOptions, ClaudeClient, ContentBlock, Message, PermissionMode, ToolResultContent,
    UserContentBlock,
};
use futures_util::{Stream, StreamExt, stream::unfold};
use serde_json::Value;
use tokio::{sync::Mutex, task::JoinHandle};
use tracing::Instrument;

use crate::{
    metadata::claude_dev_metadata,
    spans,
    tools::{
        ClaudeCompletion, ClaudeEventDto, ClaudeToolNextOutput, ClaudeToolOpenInput,
        ClaudeToolSendInput, ClaudeUserContentBlockDto,
    },
    user_input::UserInput,
};

#[derive(Debug, Clone)]
struct WorkspaceBinding {
    path: PathBuf,
    sdk_session_id: Option<String>,
}

type WorkspaceKey = (String, String);

/// Host-owned registry mapping (agent_id, workspace) to workspace path and internal SDK session id.
pub struct AgentWorkspaceRegistry {
    base_dir: PathBuf,
    state: Mutex<HashMap<WorkspaceKey, WorkspaceBinding>>,
}

impl AgentWorkspaceRegistry {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    pub async fn resolve_workspace(
        &self,
        agent_id: &str,
        workspace: Option<&str>,
    ) -> Result<(String, PathBuf)> {
        let workspace_name = normalize_workspace_name(workspace);
        let key = (agent_id.to_string(), workspace_name.clone());
        if let Some(existing) = self.state.lock().await.get(&key).cloned() {
            return Ok((workspace_name, existing.path));
        }

        let path = self.base_dir.join(agent_id).join(&workspace_name);
        std::fs::create_dir_all(&path).map_err(BamlRtError::Io)?;
        // Fallback to non-canonical path if canonicalize fails (e.g. path removed by another process).
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let mut state = self.state.lock().await;
        state.entry(key).or_insert_with(|| WorkspaceBinding {
            path: path.clone(),
            sdk_session_id: None,
        });
        Ok((workspace_name, path))
    }

    pub async fn get_sdk_session_id(&self, agent_id: &str, workspace_name: &str) -> Option<String> {
        let key = (agent_id.to_string(), workspace_name.to_string());
        self.state
            .lock()
            .await
            .get(&key)
            .and_then(|b| b.sdk_session_id.clone())
    }

    pub async fn set_sdk_session_id(
        &self,
        agent_id: &str,
        workspace_name: &str,
        sdk_session_id: String,
    ) {
        let key = (agent_id.to_string(), workspace_name.to_string());
        if let Some(binding) = self.state.lock().await.get_mut(&key) {
            binding.sdk_session_id = Some(sdk_session_id);
        }
    }
}

/// Returns a valid workspace name; empty or invalid input yields `"default"`.
pub(crate) fn normalize_workspace_name(workspace: Option<&str>) -> String {
    workspace
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("default")
        .to_string()
}

pub(crate) fn classify_result_completion(subtype: &str) -> ClaudeCompletion {
    let normalized = subtype.to_ascii_lowercase();
    if normalized.contains("awaiting_input")
        || normalized.contains("input_required")
        || normalized.contains("awaiting-user-input")
    {
        ClaudeCompletion::InputRequired
    } else if normalized.contains("interrupt") || normalized.contains("cancel") {
        ClaudeCompletion::Interrupted
    } else {
        ClaudeCompletion::Done
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeTurnRequest {
    pub prompt: Option<String>,
    pub content: Vec<UserContentBlock>,
    pub sdk_session_id: Option<String>,
}

pub type ClaudeMessageStream =
    Pin<Box<dyn Stream<Item = std::result::Result<Message, String>> + Send + 'static>>;

#[async_trait]
pub trait ClaudeStreamSource: Send + Sync {
    async fn stream_turn(
        &self,
        request: ClaudeTurnRequest,
    ) -> std::result::Result<ClaudeMessageStream, ToolSessionError>;

    async fn shutdown(&self) -> std::result::Result<(), ToolSessionError>;
}

pub trait ClaudeStreamSourceFactory: Send + Sync {
    fn create(
        &self,
        cwd: PathBuf,
    ) -> std::result::Result<Arc<dyn ClaudeStreamSource>, ToolSessionError>;
}

struct ClaudeSdkSourceFactory;

impl ClaudeStreamSourceFactory for ClaudeSdkSourceFactory {
    fn create(
        &self,
        cwd: PathBuf,
    ) -> std::result::Result<Arc<dyn ClaudeStreamSource>, ToolSessionError> {
        // Per claude-agent-sdk-rs API: cwd = working directory for CLI subprocess (subprocess runs with this cwd);
        // add_dirs = additional dirs passed as --add-dir. No other workspace option — CLI creates .claude-workspaces in cwd.
        // BypassPermissions: skip all permission prompts (unsafe; use when agent fully controls the session).
        let options = ClaudeAgentOptions::builder()
            .cwd(cwd.clone())
            .add_dirs(vec![cwd])
            .permission_mode(PermissionMode::BypassPermissions)
            .build();
        let client = ClaudeClient::try_new(options)
            .map_err(|e| ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string())))?;
        Ok(Arc::new(ClaudeSdkSource {
            client: Arc::new(Mutex::new(client)),
        }))
    }
}

struct ClaudeSdkSource {
    client: Arc<Mutex<ClaudeClient>>,
}

#[async_trait]
impl ClaudeStreamSource for ClaudeSdkSource {
    async fn stream_turn(
        &self,
        request: ClaudeTurnRequest,
    ) -> std::result::Result<ClaudeMessageStream, ToolSessionError> {
        let (tx, rx) = async_channel::unbounded::<std::result::Result<Message, String>>();
        let client = self.client.clone();
        tokio::spawn(async move {
            let mut client = client.lock().await;
            if let Err(e) = client.connect().await {
                if tx.send(Err(e.to_string())).await.is_err() {
                    tracing::debug!(
                        "claude stream: channel closed (receiver dropped) after connect error"
                    );
                }
                tx.close();
                return;
            }

            let send_res = if request.content.is_empty() {
                let prompt = request.prompt.unwrap_or_default();
                if let Some(session_id) = request.sdk_session_id {
                    client.query_with_session(prompt, session_id).await
                } else {
                    client.query(prompt).await
                }
            } else if let Some(session_id) = request.sdk_session_id {
                client
                    .query_with_content_and_session(request.content, session_id)
                    .await
            } else {
                client.query_with_content(request.content).await
            };

            if let Err(e) = send_res {
                if tx.send(Err(e.to_string())).await.is_err() {
                    tracing::debug!(
                        "claude stream: channel closed (receiver dropped) after send error"
                    );
                }
                tx.close();
                return;
            }

            let mut stream = client.receive_response();
            while let Some(next) = stream.next().await {
                match next {
                    Ok(message) => {
                        if tx.send(Ok(message)).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        if tx.send(Err(err.to_string())).await.is_err() {
                            tracing::debug!(
                                "claude stream: channel closed (receiver dropped) after stream error"
                            );
                        }
                        break;
                    }
                }
            }
            tx.close();
        });

        let stream = unfold(rx, |rx| async move {
            match rx.recv().await {
                Ok(item) => Some((item, rx)),
                Err(_) => None,
            }
        });
        Ok(Box::pin(stream))
    }

    async fn shutdown(&self) -> std::result::Result<(), ToolSessionError> {
        let mut client = self.client.lock().await;
        client
            .disconnect()
            .await
            .map_err(|e| ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string())))
    }
}

/// Max chars per thinking chunk so we stream to the client instead of one huge block.
const THINKING_CHUNK_CHARS: usize = 500;

/// Emit each event immediately; no batching (was 5, caused heavy buffering).
const MAX_EVENTS_PER_STEP: usize = 1;

fn chunk_thinking(thinking: String) -> Vec<ClaudeEventDto> {
    if thinking.len() <= THINKING_CHUNK_CHARS {
        return vec![ClaudeEventDto::AssistantThinking { thinking }];
    }
    let mut out = Vec::new();
    let mut rest = thinking.as_str();
    while !rest.is_empty() {
        let (chunk, next_rest) = if rest.len() <= THINKING_CHUNK_CHARS {
            (rest, "")
        } else {
            let split_at = rest
                .char_indices()
                .skip(THINKING_CHUNK_CHARS)
                .find(|(_, c)| *c == '\n')
                .map(|(i, _)| i)
                .unwrap_or(THINKING_CHUNK_CHARS); // No newline in first chunk chars; split at char boundary.
            let (a, b) = rest.split_at(split_at);
            (a, b.strip_prefix('\n').unwrap_or(b)) // No leading newline after split; use rest as-is.
        };
        out.push(ClaudeEventDto::AssistantThinking {
            thinking: chunk.to_string(),
        });
        rest = next_rest;
    }
    out
}

enum ClaudeQueueItem {
    Event(ClaudeEventDto),
    Completion(ClaudeCompletion),
    Error(String),
}

struct NormalizedMessage {
    events: Vec<ClaudeEventDto>,
    completion: Option<ClaudeCompletion>,
    sdk_session_id: Option<String>,
}

fn normalize_message(message: Message) -> NormalizedMessage {
    match message {
        Message::Assistant(msg) => {
            let mut events = Vec::new();
            for block in msg.message.content {
                match block {
                    ContentBlock::Text(text) => {
                        events.push(ClaudeEventDto::AssistantText { text: text.text })
                    }
                    ContentBlock::Thinking(thinking) => {
                        events.extend(chunk_thinking(thinking.thinking));
                    }
                    ContentBlock::ToolUse(tool_use) => {
                        let input = match serde_json::to_string(&tool_use.input) {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::warn!(error = %e, "tool_use input serialize failed, using empty string");
                                String::new()
                            }
                        };
                        events.push(ClaudeEventDto::AssistantToolUse {
                            id: tool_use.id,
                            name: tool_use.name,
                            input,
                        })
                    }
                    ContentBlock::ToolResult(tool_result) => {
                        let content = tool_result.content.and_then(|value| match value {
                            ToolResultContent::Text(text) => serde_json::to_string(&text).ok(),
                            ToolResultContent::Blocks(blocks) => {
                                serde_json::to_string(&blocks).ok()
                            }
                        });
                        events.push(ClaudeEventDto::AssistantToolResult {
                            tool_use_id: tool_result.tool_use_id,
                            content,
                            is_error: tool_result.is_error,
                        });
                    }
                    ContentBlock::Image(_) => {}
                }
            }
            NormalizedMessage {
                events,
                completion: None,
                sdk_session_id: msg.session_id,
            }
        }
        Message::System(msg) => {
            let data = if msg.data.is_null() {
                None
            } else {
                match serde_json::to_string(&msg.data) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!(error = %e, "system message data serialize failed");
                        None
                    }
                }
            };
            NormalizedMessage {
                events: vec![ClaudeEventDto::SystemNotice {
                    subtype: msg.subtype,
                    cwd: msg.cwd.unwrap_or_default(),
                    model: msg.model.unwrap_or_default(),
                    data,
                }],
                completion: None,
                sdk_session_id: msg.session_id,
            }
        }
        Message::StreamEvent(event) => {
            let event_json = match serde_json::to_string(&event.event) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "stream event serialize failed, using empty string");
                    String::new()
                }
            };
            NormalizedMessage {
                events: vec![ClaudeEventDto::StreamEventRaw { event: event_json }],
                completion: None,
                sdk_session_id: Some(event.session_id),
            }
        }
        Message::Result(result) => normalize_result_message(result),
        Message::User(_) | Message::ControlCancelRequest(_) => NormalizedMessage {
            events: Vec::new(),
            completion: None,
            sdk_session_id: None,
        },
    }
}

fn normalize_result_message(result: claude_agent_sdk_rs::ResultMessage) -> NormalizedMessage {
    let completion = classify_result_completion(&result.subtype);
    NormalizedMessage {
        events: vec![ClaudeEventDto::TerminalResult {
            subtype: result.subtype,
            is_error: result.is_error,
            num_turns: result.num_turns,
            total_cost_usd: result.total_cost_usd,
            result: result.result.map(|v| {
                match serde_json::to_string(&v) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "terminal result serialize failed, using empty string");
                        String::new()
                    }
                }
            }),
        }],
        completion: Some(completion),
        sdk_session_id: Some(result.session_id),
    }
}

fn map_content_block(
    block: ClaudeUserContentBlockDto,
) -> std::result::Result<UserContentBlock, ToolSessionError> {
    match block {
        ClaudeUserContentBlockDto::Text { text } => Ok(UserContentBlock::text(text)),
        ClaudeUserContentBlockDto::ImageUrl { url } => Ok(UserContentBlock::image_url(url)),
        ClaudeUserContentBlockDto::ImageBase64 { media_type, data } => {
            UserContentBlock::image_base64(media_type, data)
                .map_err(|e| ToolSessionError::Tool(ToolFailure::invalid_input(e.to_string())))
        }
    }
}

/// Claude tool bundle marker type.
pub struct Claude;

impl BundleType for Claude {
    const NAME: &'static str = "claude";

    fn description() -> &'static str {
        "Claude tools (host-managed Claude session)."
    }
}

/// Claude bundle exposing the claude/dev tool.
pub struct ClaudeSessionBundle {
    workspace_registry: Arc<AgentWorkspaceRegistry>,
    stream_factory: Arc<dyn ClaudeStreamSourceFactory>,
}

impl ClaudeSessionBundle {
    pub fn new(workspace_registry: Arc<AgentWorkspaceRegistry>) -> Self {
        Self {
            workspace_registry,
            stream_factory: Arc::new(ClaudeSdkSourceFactory),
        }
    }

    pub fn with_factory(
        workspace_registry: Arc<AgentWorkspaceRegistry>,
        stream_factory: Arc<dyn ClaudeStreamSourceFactory>,
    ) -> Self {
        Self {
            workspace_registry,
            stream_factory,
        }
    }
}

impl ToolBundle for ClaudeSessionBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = BundleName::new("claude".to_string())
            .expect("claude bundle name is a compile-time constant and must be valid");
        ToolBundleMetadata {
            name,
            description: "Claude tools (host-managed Claude session).".to_string(),
            config_schema: None,
            secret_requests: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn ToolHandler>> {
        let mut metadata = claude_dev_metadata();
        metadata.projection_semantics = Some(ToolProjectionSemantics {
            identity:
                "Stream frame identity only (event kind and lifecycle marker) without text bodies."
                    .to_string(),
            summary: "Compact hop digest: event count and completion state for this read cycle."
                .to_string(),
            detail: "Full Claude event batch for this read hop, including assistant/tool payloads."
                .to_string(),
        });
        vec![Arc::new(ClaudeSessionToolHandler {
            metadata,
            workspace_registry: self.workspace_registry.clone(),
            stream_factory: self.stream_factory.clone(),
        })]
    }
}

struct ClaudeSessionToolHandler {
    metadata: ToolFunctionMetadata,
    workspace_registry: Arc<AgentWorkspaceRegistry>,
    stream_factory: Arc<dyn ClaudeStreamSourceFactory>,
}

#[async_trait]
impl ToolHandler for ClaudeSessionToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
    }

    fn describe_invocation(&self, content: &serde_json::Value) -> String {
        let step = content.get("step").unwrap_or(content);
        let op = match step.get("op").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return "claude dev session: call".to_string(),
        };
        match op {
            "Open" => {
                let input: ClaudeToolOpenInput = step
                    .get("input")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                match input.workspace.as_deref() {
                    Some(ws) => format!("opening Claude dev session in workspace '{ws}'"),
                    None => "opening Claude dev session".to_string(),
                }
            }
            "Send" => {
                let input: ClaudeToolSendInput = step
                    .get("input")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                match input.prompt.as_deref() {
                    Some(p) if p.len() > 60 => format!("prompting Claude: '{}...'", &p[..57]),
                    Some(p) => format!("prompting Claude: '{p}'"),
                    None => "sending input to Claude dev session".to_string(),
                }
            }
            "SearchRead" | "PageRead" => "reading Claude dev session output".to_string(),
            "Finish" => "completed Claude dev session".to_string(),
            "Abort" => "aborted Claude dev session".to_string(),
            other => format!("claude dev session: {other}"),
        }
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        validate_open_input::<ClaudeToolOpenInput>(open_input.clone())?;
        let open: ClaudeToolOpenInput =
            serde_json::from_value(open_input).map_err(baml_rt_core::BamlRtError::Json)?;

        let agent_id = ctx.agent_id.as_str().to_string();
        let (workspace_name, cwd) = self
            .workspace_registry
            .resolve_workspace(&agent_id, open.workspace.as_deref())
            .await?;
        let session_id_str = ctx.session_id.to_string();
        let workspace_base = self.workspace_registry.base_dir();
        tracing::debug!(
            session_id = %session_id_str,
            agent_id = %agent_id,
            workspace = %workspace_name,
            workspace_base = %workspace_base.display(),
            cwd = %cwd.display(),
            "claude/dev session open: cwd (and workspace_base) passed to SDK as working directory; CLI writes under cwd",
        );
        let _guard = spans::session_open(&session_id_str, &agent_id, &workspace_name).entered();
        let stream_source = self.stream_factory.create(cwd).map_err(|err| match err {
            ToolSessionError::Transport(inner) => inner,
            ToolSessionError::Tool(failure) => BamlRtError::InvalidArgument(format!(
                "Tool failure ({:?}): {}",
                failure.kind, failure.message
            )),
        })?;

        Ok(Box::new(ClaudeSession {
            ctx,
            agent_id,
            workspace_name,
            workspace_registry: self.workspace_registry.clone(),
            stream_source,
            pending: VecDeque::new(),
            output_rx: None,
            stream_handle: None,
            closed: false,
            awaiting_input: false,
            read_hop: 0,
        }))
    }
}

struct ClaudeSession {
    ctx: ToolSessionContext,
    agent_id: String,
    workspace_name: String,
    workspace_registry: Arc<AgentWorkspaceRegistry>,
    stream_source: Arc<dyn ClaudeStreamSource>,
    pending: VecDeque<ClaudeQueueItem>,
    output_rx: Option<async_channel::Receiver<ClaudeQueueItem>>,
    stream_handle: Option<JoinHandle<()>>,
    closed: bool,
    awaiting_input: bool,
    read_hop: u32,
}

impl ClaudeSession {
    fn ensure_open(&self) -> std::result::Result<(), ToolSessionError> {
        if self.closed {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "Claude session {} is closed",
                self.ctx.session_id
            ))));
        }
        Ok(())
    }
}

#[async_trait]
impl ToolSession for ClaudeSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        self.ensure_open()?;
        if self.output_rx.is_some() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "claude/dev session already has an active turn; call next() until Suspended or Done before sending again".to_string(),
            )));
        }

        let send_input: ClaudeToolSendInput = serde_json::from_value(input).map_err(|e| {
            ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "Invalid claude/dev send input: {e}"
            )))
        })?;

        let mut content = Vec::new();
        if let Some(prompt) = send_input.prompt.clone() {
            let trimmed = prompt.trim().to_string();
            if !trimmed.is_empty() {
                content.push(UserContentBlock::text(trimmed));
            }
        }
        if let Some(blocks) = send_input.content {
            for block in blocks {
                content.push(map_content_block(block)?);
            }
        }
        // When agent sends structured approval (UserInput::ToolApproval), use the right type: send its display text as the message when no other content; session applies permission (BypassPermissions or future callback).
        if content.is_empty()
            && let Some(ref user_input_value) = send_input.user_input
            && let Ok(user_input) = serde_json::from_value::<UserInput>(user_input_value.clone())
        {
            let text = user_input.display_text();
            if !text.trim().is_empty() {
                content.push(UserContentBlock::text(text));
            }
        }

        if content.is_empty() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "claude/dev send requires prompt, content, or userInput".to_string(),
            )));
        }

        let sdk_session_id = self
            .workspace_registry
            .get_sdk_session_id(&self.agent_id, &self.workspace_name)
            .await;
        let request = ClaudeTurnRequest {
            prompt: None,
            content,
            sdk_session_id,
        };

        let (tx, rx) = async_channel::unbounded::<ClaudeQueueItem>();
        self.output_rx = Some(rx);
        self.awaiting_input = false;

        let session_id_str = self.ctx.session_id.to_string();
        let send_span = spans::session_send(&session_id_str);
        let _send_guard = send_span.entered();

        let source = self.stream_source.clone();
        let registry = self.workspace_registry.clone();
        let agent_id = self.agent_id.clone();
        let workspace_name = self.workspace_name.clone();
        let session_id_for_task = session_id_str.clone();
        let consumer_span = spans::stream_turn_consumer(&session_id_for_task);
        let handle = tokio::spawn(
            async move {
                tracing::debug!(
                    session_id = %session_id_for_task,
                    "claude stream: turn started",
                );
                let stream_result = source.stream_turn(request).await;
                let mut stream = match stream_result {
                    Ok(stream) => {
                        tracing::debug!(
                            session_id = %session_id_for_task,
                            "claude stream: stream opened",
                        );
                        stream
                    }
                    Err(err) => {
                        tracing::warn!(
                            session_id = %session_id_for_task,
                            err = ?err,
                            "claude stream: turn failed",
                        );
                        if tx.send(ClaudeQueueItem::Error(format!("{err:?}"))).await.is_err() {
                            tracing::debug!(
                                session_id = %session_id_for_task,
                                "claude stream: channel closed (receiver dropped) after turn failure",
                            );
                        }
                        tx.close();
                        return;
                    }
                };

                let mut message_count: u32 = 0;
                while let Some(item) = stream
                    .next()
                    .instrument(spans::stream_next_await(&session_id_for_task))
                    .await
                {
                    match item {
                        Ok(message) => {
                            message_count += 1;
                            let normalized = normalize_message(message);
                            if let Some(sdk_session_id) = normalized.sdk_session_id {
                                registry
                                    .set_sdk_session_id(&agent_id, &workspace_name, sdk_session_id)
                                    .await;
                            }
                            let n = normalized.events.len();
                            for event in normalized.events {
                                if tx.send(ClaudeQueueItem::Event(event)).await.is_err() {
                                    tracing::debug!(
                                        session_id = %session_id_for_task,
                                        "claude stream: channel closed (receiver dropped)",
                                    );
                                    return;
                                }
                            }
                            if message_count == 1 {
                                tracing::debug!(
                                    session_id = %session_id_for_task,
                                    event_count = n,
                                    "claude stream: first message received",
                                );
                            } else {
                                tracing::debug!(
                                    session_id = %session_id_for_task,
                                    message_count,
                                    event_count = n,
                                    "claude stream: message received",
                                );
                            }
                            if let Some(completion) = normalized.completion {
                                tracing::debug!(
                                    session_id = %session_id_for_task,
                                    completion = ?completion,
                                    message_count,
                                    "claude stream: completion",
                                );
                                if tx.send(ClaudeQueueItem::Completion(completion)).await.is_err() {
                                    tracing::debug!(
                                        session_id = %session_id_for_task,
                                        "claude stream: channel closed (receiver dropped) after completion",
                                    );
                                }
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                session_id = %session_id_for_task,
                                err = ?err,
                                "claude stream: stream error",
                            );
                            if tx.send(ClaudeQueueItem::Error(err)).await.is_err() {
                                tracing::debug!(
                                    session_id = %session_id_for_task,
                                    "claude stream: channel closed (receiver dropped) after stream error",
                                );
                            }
                            break;
                        }
                    }
                }
                tracing::debug!(
                    session_id = %session_id_for_task,
                    "claude stream: closing channel",
                );
                tx.close();
            }
            .instrument(consumer_span),
        );
        self.stream_handle = Some(handle);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        self.ensure_open()?;
        let session_id_str = self.ctx.session_id.to_string();
        let next_span = spans::session_next(&session_id_str);
        let _next_guard = next_span.entered();

        if let Some(item) = self.pending.pop_front() {
            self.pending.push_front(item);
            tracing::debug!(
                session_id = %session_id_str,
                "claude next: serving from pending",
            );
        } else if let Some(rx) = &self.output_rx {
            drop(_next_guard);
            tracing::debug!(
                session_id = %session_id_str,
                "claude next: waiting on channel",
            );
            let item = rx
                .recv()
                .instrument(spans::session_next_recv_await(&session_id_str))
                .await;
            match &item {
                Ok(_) => {
                    tracing::debug!(
                        session_id = %session_id_str,
                        "claude next: received from channel",
                    );
                }
                Err(_) => {
                    tracing::debug!(
                        session_id = %session_id_str,
                        "claude next: channel closed",
                    );
                }
            }
            match item {
                Ok(item) => self.pending.push_back(item),
                Err(_) => {
                    self.output_rx = None;
                }
            }
        }

        if self.pending.is_empty() {
            if self.awaiting_input {
                self.read_hop = self.read_hop.saturating_add(1);
                let output = ClaudeToolNextOutput {
                    events: Vec::new(),
                    completion: Some(ClaudeCompletion::InputRequired),
                    history_context: Some(HistoryContextV1 {
                        hop: self.read_hop,
                        op: HistoryContextSessionOp::PageRead,
                        status: HistoryContextStatus::Suspended,
                        truncated: false,
                        cursor: None,
                        payload: Some(opaque_json_map_from_object(serde_json::json!({
                            "eventCount": 0,
                            "completion": "INPUT_REQUIRED",
                        }))),
                    }),
                };
                let value = serde_json::to_value(output).map_err(|e| {
                    ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string()))
                })?;
                return Ok(ToolStep::Suspended { output: value });
            }
            return Ok(ToolStep::Done { output: None });
        }

        let mut events = Vec::new();
        let mut completion = None;

        while events.len() < MAX_EVENTS_PER_STEP {
            if let Some(item) = self.pending.pop_front() {
                match item {
                    ClaudeQueueItem::Event(event) => events.push(event),
                    ClaudeQueueItem::Completion(c) => {
                        completion = Some(c);
                        break;
                    }
                    ClaudeQueueItem::Error(message) => {
                        self.output_rx = None;
                        return Ok(ToolStep::Error {
                            error: ToolFailure::execution_failed(message),
                        });
                    }
                }
            } else if let Some(rx) = &self.output_rx {
                match rx.try_recv() {
                    Ok(next_item) => self.pending.push_back(next_item),
                    Err(async_channel::TryRecvError::Empty) => break,
                    Err(async_channel::TryRecvError::Closed) => {
                        self.output_rx = None;
                        break;
                    }
                }
            } else {
                break;
            }
        }

        let event_count = events.len();
        self.read_hop = self.read_hop.saturating_add(1);
        let output = ClaudeToolNextOutput {
            events,
            completion: completion.clone(),
            history_context: Some(HistoryContextV1 {
                hop: self.read_hop,
                op: HistoryContextSessionOp::PageRead,
                status: match &completion {
                    Some(ClaudeCompletion::InputRequired) => HistoryContextStatus::Suspended,
                    Some(ClaudeCompletion::Done) | Some(ClaudeCompletion::Interrupted) => {
                        HistoryContextStatus::Done
                    }
                    None => HistoryContextStatus::Streaming,
                },
                truncated: false,
                cursor: None,
                payload: Some(opaque_json_map_from_object(serde_json::json!({
                    "eventCount": event_count,
                    "completion": completion.as_ref().map(|c| format!("{:?}", c)),
                }))),
            }),
        };
        let value = serde_json::to_value(output)
            .map_err(|e| ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string())))?;

        let step_kind = match &completion {
            Some(ClaudeCompletion::InputRequired) => "Suspended",
            Some(ClaudeCompletion::Done) | Some(ClaudeCompletion::Interrupted) => "Done",
            None => "Streaming",
        };
        tracing::debug!(
            session_id = %session_id_str,
            step = step_kind,
            event_count,
            "claude next: step",
        );

        match completion {
            Some(ClaudeCompletion::InputRequired) => {
                self.awaiting_input = true;
                self.output_rx = None;
                Ok(ToolStep::Suspended { output: value })
            }
            Some(ClaudeCompletion::Done) | Some(ClaudeCompletion::Interrupted) => {
                self.awaiting_input = false;
                self.output_rx = None;
                Ok(ToolStep::Done {
                    output: Some(value),
                })
            }
            None => Ok(ToolStep::Streaming { output: value }),
        }
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        if let Some(handle) = self.stream_handle.take() {
            handle.abort();
        }
        if let Err(e) = self.stream_source.shutdown().await {
            tracing::warn!(error = ?e, "claude session stream shutdown failed (finish)");
        }
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        if let Some(handle) = self.stream_handle.take() {
            handle.abort();
        }
        if let Err(e) = self.stream_source.shutdown().await {
            tracing::warn!(error = ?e, "claude session stream shutdown failed (abort)");
        }
        Ok(())
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        if let Some(handle) = self.stream_handle.take() {
            handle.abort();
        }
    }
}
