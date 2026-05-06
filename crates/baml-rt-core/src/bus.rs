//! Stream-first command/event/effect bus for runtime orchestration.
//!
//! This is the canonical transport-agnostic boundary for runtime observation and
//! liveness. All command, domain-event, and effect signals flow as envelopes.
//!
//! ## Provenance Effect Invariants
//!
//! 1. **Single source of truth**:
//!    All provenance writes flow through `EffectEvent` variants.
//!    `ProvenanceEffectSubscriber` is the sole writer; interceptors observe but do not emit.
//!
//! 2. **Token lifecycle** (type-level enforcement):
//!    ∀ `EffectStartToken<K>`: exactly one `complete()` call consumes the token by value.
//!    `Drop` impl warns on leaked tokens (started but never completed).
//!
//! 3. **Result payload completeness**:
//!    `ToolCompleted.result` carries the `ToolStep::Done { output }` value.
//!    `LlmCompleted.result_payload` carries the LLM completion payload.
//!    Both are `Option<Value>` — `None` only for error/abort paths.

use std::{collections::HashMap, marker::PhantomData, pin::Pin, sync::Arc};

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use futures_util::{future::join_all, stream::Stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::{
    clock_events,
    context::RuntimeScope,
    correlation,
    ids::{AgentId, ContextId, CorrelationId, IntentId, PlanId, PlanStepId, TaskId},
    semantics::Outcome,
};

pub type BusStream<T> = Pin<Box<dyn Stream<Item = T> + Send + 'static>>;

fn receiver_stream<T: Send + 'static>(rx: Receiver<T>) -> BusStream<T> {
    Box::pin(async_stream::stream! {
        while let Ok(item) = rx.recv().await {
            yield item;
        }
    })
}

/// What kind of effect is being executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    Tool,
    Llm,
    A2a,
}

impl EffectKind {
    fn underflow_message(&self) -> &'static str {
        match self {
            EffectKind::Tool => {
                "Effect count underflow: ToolCompleted without matching ToolStarted"
            }
            EffectKind::Llm => "Effect count underflow: LlmCompleted without matching LlmStarted",
            EffectKind::A2a => "Effect count underflow: A2aCompleted without matching A2aStarted",
        }
    }
}

pub struct ToolKind;
pub struct LlmKind;
pub struct A2aKind;

#[derive(Debug, Clone)]
pub struct ToolEffectMetadata {
    pub tool_name: String,
    pub function_name: Option<String>,
    pub args: serde_json::Value,
    pub metadata: serde_json::Value,
    /// For system/internal_a2a: the delegated-to agent package (write-time provenance).
    pub delegation_target: Option<String>,
    /// Execution backend that serviced this invocation (e.g. "InProcess", "ExternalProcess").
    /// `None` when emitter did not populate it — provenance records None in that case.
    pub tool_backend: Option<String>,
    /// Content-addressed digest of the tool artifact (external tools only).
    /// Always `None` in dev mode; Phase 2 OCI-pinned path populates this.
    pub tool_digest: Option<String>,
}

/// How the tool_name was resolved for an LLM effect.
#[derive(Debug, Clone)]
pub enum ToolNameResolution {
    /// Resolved from the function-tool manifest at pre-execution time.
    /// Canonical path for session plan functions.
    FromManifest(String),
    /// Extracted from the LLM result payload (tool_name field in JSON).
    FromPayload(String),
    /// This LLM function does not produce a tool invocation.
    NotApplicable,
}

impl ToolNameResolution {
    pub fn as_tool_name(&self) -> Option<&str> {
        match self {
            Self::FromManifest(s) | Self::FromPayload(s) => Some(s),
            Self::NotApplicable => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmEffectMetadata {
    pub client: String,
    pub model: String,
    pub function_name: String,
    pub prompt: serde_json::Value,
    pub metadata: serde_json::Value,
    /// Tool name resolution for this LLM call. Used by drift scoring to
    /// route `describe_invocation` to the correct tool handler.
    pub tool_name: ToolNameResolution,
}

impl ToolEffectMetadata {
    pub fn has_message_id(&self) -> bool {
        self.metadata
            .get("message_id")
            .and_then(|v| v.as_str())
            .is_some()
    }
}

impl LlmEffectMetadata {
    pub fn has_message_id(&self) -> bool {
        self.metadata
            .get("message_id")
            .and_then(|v| v.as_str())
            .is_some()
    }
}

#[derive(Debug, Clone)]
pub enum A2aLivenessRole {
    /// Command envelope lifecycle (ingress/egress orchestration).
    Command,
    /// Progress-capable A2A effect work (e.g. nested or child transport work).
    Effect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningSupersessionKind {
    ReplacedBy,
    RefinedBy,
}

#[derive(Debug, Clone)]
pub struct A2aEffectMetadata {
    pub agent_id: AgentId,
    pub method: String,
    pub request_id: Option<String>,
    pub liveness_role: A2aLivenessRole,
    pub metadata: serde_json::Value,
}

/// An individual operation within a tool session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SessionStepOp {
    /// Session opened — tool session is now active, next op is Send.
    Open,
    /// Blocking Send completed — result archived at `archive_ref` (e.g. `"@1"`).
    /// `header` is the full display string: `"@1 · \"summary\" · NL · size"`.
    SendDone {
        archive_ref: String,
        header: String,
        informed_by: String,
    },
    /// LLM searched the archive at `archive_ref` with a line filter (`grep` pattern).
    SearchRead {
        archive_ref: String,
        grep: String,
        offset: usize,
        limit: usize,
    },
    /// LLM paged the archive at `archive_ref` without a line filter (contiguous window).
    PageRead {
        archive_ref: String,
        offset: usize,
        limit: usize,
    },
}

#[derive(Debug, Clone)]
pub enum EffectEvent {
    ToolStarted {
        context_id: ContextId,
        metadata: ToolEffectMetadata,
    },
    ToolCompleted {
        context_id: ContextId,
        metadata: ToolEffectMetadata,
        duration_ms: u64,
        outcome: Outcome,
        /// Tool result payload when available (e.g. the Read output for a session).
        result: Option<serde_json::Value>,
    },
    LlmStarted {
        context_id: ContextId,
        metadata: LlmEffectMetadata,
    },
    LlmCompleted {
        context_id: ContextId,
        metadata: LlmEffectMetadata,
        usage: Option<LlmUsage>,
        /// Serialized LLM completion payload when available.
        result_payload: Option<Value>,
        duration_ms: u64,
        outcome: Outcome,
        /// When outcome is Failure, optional reason (e.g. plan extraction failure) for provenance PromptRejected.
        rejection_reason: Option<String>,
    },
    A2aStarted {
        context_id: ContextId,
        metadata: A2aEffectMetadata,
    },
    A2aCompleted {
        context_id: ContextId,
        metadata: A2aEffectMetadata,
        duration_ms: u64,
        outcome: Outcome,
    },
    /// Intent committed for a task. **Citations** are ref-table strings (`#N` history, `@N` archive, …),
    /// not opaque “evidence” prose — they tie the intent to **citable conversation history** as well as
    /// archives so provenance and drift checks can validate grounding (see `docs/citable-history-and-checked-citations.md`).
    IntentResolved {
        context_id: ContextId,
        task_id: TaskId,
        intent_id: IntentId,
        description: String,
        /// Ref-table citations co-emitted with the intent; validated at construction (`Citation`).
        citations: Vec<crate::Citation>,
        supersession: Option<PlanningSupersessionKind>,
        /// Lineage epoch when intent was resolved; for provenance and consistency checks.
        epoch: Option<u64>,
    },
    PlanGenerated {
        context_id: ContextId,
        task_id: TaskId,
        intent_id: IntentId,
        plan_id: PlanId,
        steps: Value,
        supersession: Option<PlanningSupersessionKind>,
        /// Lineage epoch when plan was generated; for provenance and consistency checks.
        epoch: Option<u64>,
    },
    /// Plan step lifecycle transition. **Citations** ground the transition in the same ref-table vocabulary
    /// as LLM outputs (`#N` / `@N`), enabling checked provenance rather than uninterpreted evidence strings.
    PlanStepStatusChanged {
        context_id: ContextId,
        task_id: TaskId,
        intent_id: IntentId,
        plan_id: PlanId,
        step_id: PlanStepId,
        old_status: Option<String>,
        new_status: String,
        /// Ref-table citations for this transition; stored on PlanStep provenance for audit/drift.
        citations: Vec<crate::Citation>,
        /// Lineage epoch when step status changed; for provenance and consistency checks.
        epoch: Option<u64>,
    },
    /// Tool stream chunk (Streaming/Suspended step). Relay to HTTP A2A session immediately so the UI sees it; PROV is already recorded via the tool interceptor.
    ToolStreamChunk { context_id: ContextId, chunk: Value },
    /// An individual step within a tool session — emitted for each meaningful op.
    /// Enables conversation_context to surface session state between executor hops
    /// without waiting for the session to close.
    ToolSessionStep {
        context_id: ContextId,
        tool_name: String,
        session_id: String,
        op: SessionStepOp,
        /// When set, provenance ties the session step to this task (task-scoped episode transcript).
        task_id: Option<TaskId>,
    },
}

#[derive(Debug, Clone)]
pub enum LlmUsage {
    Known {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        cached_input_tokens: Option<u64>,
    },
    Unknown,
}

impl EffectEvent {
    pub fn kind(&self) -> EffectKind {
        match self {
            EffectEvent::ToolStarted { .. }
            | EffectEvent::ToolCompleted { .. }
            | EffectEvent::ToolStreamChunk { .. }
            | EffectEvent::ToolSessionStep { .. } => EffectKind::Tool,
            EffectEvent::LlmStarted { .. } | EffectEvent::LlmCompleted { .. } => EffectKind::Llm,
            EffectEvent::A2aStarted { .. }
            | EffectEvent::A2aCompleted { .. }
            | EffectEvent::IntentResolved { .. }
            | EffectEvent::PlanGenerated { .. }
            | EffectEvent::PlanStepStatusChanged { .. } => EffectKind::A2a,
        }
    }

    pub fn context_id(&self) -> &ContextId {
        match self {
            EffectEvent::ToolStarted { context_id, .. }
            | EffectEvent::ToolCompleted { context_id, .. }
            | EffectEvent::ToolStreamChunk { context_id, .. }
            | EffectEvent::ToolSessionStep { context_id, .. } => context_id,
            EffectEvent::LlmStarted { context_id, .. }
            | EffectEvent::LlmCompleted { context_id, .. } => context_id,
            EffectEvent::A2aStarted { context_id, .. }
            | EffectEvent::A2aCompleted { context_id, .. }
            | EffectEvent::IntentResolved { context_id, .. }
            | EffectEvent::PlanGenerated { context_id, .. }
            | EffectEvent::PlanStepStatusChanged { context_id, .. } => context_id,
        }
    }
}

pub struct EffectStartToken<K> {
    context_id: Option<ContextId>,
    metadata: Option<EffectStartMetadata>,
    _kind: PhantomData<K>,
}

enum EffectStartMetadata {
    Tool(ToolEffectMetadata),
    Llm(LlmEffectMetadata),
    A2a(A2aEffectMetadata),
}

fn take_token_parts(
    context_id: &mut Option<ContextId>,
    metadata: &mut Option<EffectStartMetadata>,
) -> (ContextId, EffectStartMetadata) {
    (
        context_id.take().expect("token already completed"),
        metadata.take().expect("token already completed"),
    )
}

impl<K> Drop for EffectStartToken<K> {
    fn drop(&mut self) {
        if self.context_id.is_some() || self.metadata.is_some() {
            // Log a warning but do NOT panic. Async cancellation (e.g. tokio::time::timeout
            // dropping a future mid-await) is a valid operation — the token may be dropped
            // before completion. Panicking in Drop causes double-panics and process aborts
            // which are worse than a missing provenance record.
            tracing::warn!(
                context_id = ?self.context_id,
                kind = std::any::type_name::<K>(),
                "EffectStartToken dropped without completion (possible async cancellation)"
            );
            // Keep the release-mode error log for visibility without the panic.
            #[cfg(not(debug_assertions))]
            {
                tracing::error!(
                    context_id = ?self.context_id,
                    kind = std::any::type_name::<K>(),
                    "EffectStartToken dropped without completion - effect leak"
                );
            }
        }
    }
}

impl EffectStartToken<ToolKind> {
    /// INVARIANT 2 (token lifecycle): Consumes the token by value — exactly one completion per start.
    pub async fn complete(
        mut self,
        emitter: &dyn EffectEmitter,
        duration_ms: u64,
        outcome: Outcome,
        result: Option<serde_json::Value>,
    ) -> crate::Result<()> {
        let (context_id, metadata) = take_token_parts(&mut self.context_id, &mut self.metadata);
        let metadata = match metadata {
            EffectStartMetadata::Tool(meta) => meta,
            _ => unreachable!(),
        };
        emitter
            .emit(EffectEvent::ToolCompleted {
                context_id,
                metadata,
                duration_ms,
                outcome,
                result,
            })
            .await
    }
}

impl EffectStartToken<LlmKind> {
    pub async fn complete(
        mut self,
        emitter: &dyn EffectEmitter,
        usage: Option<LlmUsage>,
        result_payload: Option<Value>,
        duration_ms: u64,
        outcome: Outcome,
        rejection_reason: Option<String>,
    ) -> crate::Result<()> {
        let (context_id, metadata) = take_token_parts(&mut self.context_id, &mut self.metadata);
        let mut metadata = match metadata {
            EffectStartMetadata::Llm(meta) => meta,
            _ => unreachable!(),
        };
        // If tool_name wasn't resolved from the manifest, try the payload as fallback
        // for single-shot tool calls that embed tool_name in their result.
        if matches!(metadata.tool_name, ToolNameResolution::NotApplicable)
            && let Some(ref payload) = result_payload
            && let Some(name) = extract_tool_name(payload)
        {
            metadata.tool_name = ToolNameResolution::FromPayload(name);
        }
        emitter
            .emit(EffectEvent::LlmCompleted {
                context_id,
                metadata,
                usage,
                result_payload,
                duration_ms,
                outcome,
                rejection_reason,
            })
            .await
    }
}

/// Extract tool name from a BAML-parsed LLM result payload.
/// Handles:
/// - `{"tool_name": "..."}` (direct tool call)
/// - `{Variant: {"tool_name": "..."}}` (wrapped variant)
/// - `{"step": {"tool_name": "...", "op": "Open", ...}}` (polymorphic session Open)
fn extract_tool_name(payload: &Value) -> Option<String> {
    let obj = payload.as_object()?;
    if let Some(name) = obj.get("tool_name").and_then(Value::as_str) {
        return Some(name.to_string());
    }
    if let Some(step) = obj.get("step")
        && let Some(name) = step.get("tool_name").and_then(Value::as_str)
    {
        return Some(name.to_string());
    }
    if obj.len() == 1 {
        let (_, inner) = obj.iter().next()?;
        if let Some(name) = inner.get("tool_name").and_then(Value::as_str) {
            return Some(name.to_string());
        }
    }
    None
}

impl EffectStartToken<A2aKind> {
    pub async fn complete(
        mut self,
        emitter: &dyn EffectEmitter,
        duration_ms: u64,
        outcome: Outcome,
    ) -> crate::Result<()> {
        let (context_id, metadata) = take_token_parts(&mut self.context_id, &mut self.metadata);
        let metadata = match metadata {
            EffectStartMetadata::A2a(meta) => meta,
            _ => unreachable!(),
        };
        emitter
            .emit(EffectEvent::A2aCompleted {
                context_id,
                metadata,
                duration_ms,
                outcome,
            })
            .await
    }
}

#[async_trait]
pub trait EffectEmitter: EffectLiveness + Send + Sync {
    async fn emit(&self, event: EffectEvent) -> crate::Result<()>;

    /// Register an effect subscriber. Default: no-op. BusWithEffects overrides to fan out.
    async fn subscribe_effect_subscriber(&self, _subscriber: Arc<dyn EffectSubscriber>) {}

    async fn start_tool(
        &self,
        context_id: ContextId,
        metadata: ToolEffectMetadata,
    ) -> crate::Result<EffectStartToken<ToolKind>> {
        let token = EffectStartToken {
            context_id: Some(context_id.clone()),
            metadata: Some(EffectStartMetadata::Tool(metadata.clone())),
            _kind: PhantomData,
        };
        self.emit(EffectEvent::ToolStarted {
            context_id,
            metadata,
        })
        .await?;
        Ok(token)
    }

    async fn start_llm(
        &self,
        context_id: ContextId,
        metadata: LlmEffectMetadata,
    ) -> crate::Result<EffectStartToken<LlmKind>> {
        let token = EffectStartToken {
            context_id: Some(context_id.clone()),
            metadata: Some(EffectStartMetadata::Llm(metadata.clone())),
            _kind: PhantomData,
        };
        self.emit(EffectEvent::LlmStarted {
            context_id,
            metadata,
        })
        .await?;
        Ok(token)
    }

    async fn start_a2a(
        &self,
        context_id: ContextId,
        metadata: A2aEffectMetadata,
    ) -> crate::Result<EffectStartToken<A2aKind>> {
        let token = EffectStartToken {
            context_id: Some(context_id.clone()),
            metadata: Some(EffectStartMetadata::A2a(metadata.clone())),
            _kind: PhantomData,
        };
        self.emit(EffectEvent::A2aStarted {
            context_id,
            metadata,
        })
        .await?;
        Ok(token)
    }
}

#[async_trait]
pub trait EffectLiveness: Send + Sync {
    async fn in_flight(&self, context_id: &ContextId) -> InFlightCounts;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InFlightCounts {
    pub tool: u32,
    pub llm: u32,
    /// Progress-capable A2A effects.
    pub a2a: u32,
    /// Command envelope lifecycle counters (non-progress for timeout gating).
    pub a2a_command: u32,
}

impl InFlightCounts {
    pub fn any(&self) -> bool {
        self.tool > 0 || self.llm > 0 || self.a2a > 0 || self.a2a_command > 0
    }

    /// True iff there is downstream work that can advance the current poll loop.
    pub fn has_progress_effects(&self) -> bool {
        self.tool > 0 || self.llm > 0 || self.a2a > 0
    }

    pub fn total(&self) -> u32 {
        self.tool + self.llm + self.a2a + self.a2a_command
    }

    pub fn get_mut(&mut self, kind: EffectKind) -> &mut u32 {
        match kind {
            EffectKind::Tool => &mut self.tool,
            EffectKind::Llm => &mut self.llm,
            EffectKind::A2a => &mut self.a2a,
        }
    }
}

/// Whether `EffectEmitter::emit` should await an `EffectSubscriber` before
/// returning, or run it detached.
///
/// `Awaitable` is the contract that downstream reads depend on. The canonical
/// example is `ProvenanceEffectSubscriber`: its Surreal write must be
/// committed before the next `conversation_context` read fires, so `emit()`
/// has to await it.
///
/// `Background` is for pure observability work (drift scoring, status
/// updates, SSE relays) where no read path structurally depends on
/// completion. Spawning these subscribers keeps their cost off the
/// user-facing critical path of an LLM hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectSubscriberTier {
    Awaitable,
    Background,
}

#[async_trait]
pub trait EffectSubscriber: Send + Sync {
    async fn on_effect(&self, event: &EffectEvent) -> crate::Result<()>;

    /// Defaults to `Background` so adding a new subscriber does not silently
    /// drag observability work onto the user-facing critical path of an LLM
    /// completion. Override to `Awaitable` only when downstream reads
    /// structurally depend on this subscriber's work being visible.
    fn tier(&self) -> EffectSubscriberTier {
        EffectSubscriberTier::Background
    }
}

/// Transport-agnostic command envelope payload.
#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub metadata: Value,
    pub input: Value,
}

/// Transport-agnostic domain event payload.
#[derive(Debug, Clone)]
pub struct DomainEvent {
    pub name: String,
    pub metadata: Value,
    pub output: Value,
    pub terminal: bool,
}

/// Payload emitted on the bus.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Payload {
    Command(Command),
    DomainEvent(DomainEvent),
    Effect(EffectEvent),
}

/// Canonical envelope emitted by runtime paths.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub scope: Option<RuntimeScope>,
    pub correlation_id: Option<CorrelationId>,
    pub timestamp_ms: u64,
    pub payload: Payload,
}

impl Envelope {
    pub fn now(scope: Option<RuntimeScope>, payload: Payload) -> Self {
        Self {
            scope,
            correlation_id: correlation::current_correlation_id(),
            timestamp_ms: crate::now_unix_ms(clock_events::BUS_ENVELOPE),
            payload,
        }
    }
}

#[async_trait]
pub trait Subscriber: Send + Sync {
    async fn on_envelope(&self, envelope: &Envelope) -> crate::Result<()>;
}

#[async_trait]
pub trait BusApi: Send + Sync {
    async fn emit(&self, envelope: Envelope) -> crate::Result<()>;
    async fn subscribe(&self, subscriber: Arc<dyn Subscriber>);
    async fn stream(&self) -> BusStream<Envelope>;
}

/// Canonical runtime boundary: envelope bus + effect lifecycle + liveness.
pub trait EffectRuntime: BusApi + EffectEmitter + EffectLiveness + Send + Sync {}

impl<T> EffectRuntime for T where T: BusApi + EffectEmitter + EffectLiveness + Send + Sync {}

/// In-memory fanout bus for envelopes.
pub struct Bus {
    subscribers: Arc<RwLock<Vec<Arc<dyn Subscriber>>>>,
    streams: Arc<RwLock<Vec<Sender<Envelope>>>>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(Vec::new())),
            streams: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BusApi for Bus {
    async fn emit(&self, envelope: Envelope) -> crate::Result<()> {
        let subscribers = self.subscribers.read().await;
        for subscriber in subscribers.iter() {
            subscriber.on_envelope(&envelope).await?;
        }

        let streams = self.streams.read().await.clone();
        for tx in streams {
            if tx.send(envelope.clone()).await.is_err() {
                tracing::debug!("bus envelope send failed (receiver dropped)");
            }
        }
        let mut stream_guard = self.streams.write().await;
        stream_guard.retain(|tx| !tx.is_closed());
        Ok(())
    }

    async fn subscribe(&self, subscriber: Arc<dyn Subscriber>) {
        let mut subscribers = self.subscribers.write().await;
        subscribers.push(subscriber);
    }

    async fn stream(&self) -> BusStream<Envelope> {
        let (tx, rx) = async_channel::unbounded();
        let mut streams = self.streams.write().await;
        streams.push(tx);
        receiver_stream(rx)
    }
}

/// Bus that also maintains effect liveness and effect-subscriber fanout.
pub struct BusWithEffects {
    bus: Bus,
    counts: Arc<RwLock<HashMap<ContextId, InFlightCounts>>>,
    effect_subscribers: Arc<RwLock<Vec<Arc<dyn EffectSubscriber>>>>,
}

impl BusWithEffects {
    pub fn new() -> Self {
        Self {
            bus: Bus::new(),
            counts: Arc::new(RwLock::new(HashMap::new())),
            effect_subscribers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn subscribe_effect(&self, subscriber: Arc<dyn EffectSubscriber>) {
        let mut subs = self.effect_subscribers.write().await;
        subs.push(subscriber);
    }

    async fn process_effect(&self, event: EffectEvent) -> crate::Result<()> {
        let context_id = event.context_id().clone();
        {
            let mut counts = self.counts.write().await;
            let entry = counts
                .entry(context_id.clone())
                .or_insert_with(InFlightCounts::default);
            match event {
                EffectEvent::ToolStarted { .. } => entry.tool = entry.tool.saturating_add(1),
                EffectEvent::LlmStarted { .. } => entry.llm = entry.llm.saturating_add(1),
                EffectEvent::A2aStarted { ref metadata, .. } => match metadata.liveness_role {
                    A2aLivenessRole::Command => {
                        entry.a2a_command = entry.a2a_command.saturating_add(1)
                    }
                    A2aLivenessRole::Effect => entry.a2a = entry.a2a.saturating_add(1),
                },
                EffectEvent::ToolCompleted { .. } => {
                    if entry.tool == 0 {
                        tracing::error!(
                            context_id = ?context_id,
                            "{}",
                            EffectKind::Tool.underflow_message()
                        );
                    }
                    entry.tool = entry.tool.saturating_sub(1);
                }
                EffectEvent::LlmCompleted { .. } => {
                    if entry.llm == 0 {
                        tracing::error!(
                            context_id = ?context_id,
                            "{}",
                            EffectKind::Llm.underflow_message()
                        );
                    }
                    entry.llm = entry.llm.saturating_sub(1);
                }
                EffectEvent::A2aCompleted { ref metadata, .. } => match metadata.liveness_role {
                    A2aLivenessRole::Command => {
                        if entry.a2a_command == 0 {
                            tracing::error!(
                                context_id = ?context_id,
                                "Effect count underflow: A2aCommandCompleted without matching A2aCommandStarted"
                            );
                        }
                        entry.a2a_command = entry.a2a_command.saturating_sub(1);
                    }
                    A2aLivenessRole::Effect => {
                        if entry.a2a == 0 {
                            tracing::error!(
                                context_id = ?context_id,
                                "{}",
                                EffectKind::A2a.underflow_message()
                            );
                        }
                        entry.a2a = entry.a2a.saturating_sub(1);
                    }
                },
                EffectEvent::IntentResolved { .. }
                | EffectEvent::PlanGenerated { .. }
                | EffectEvent::PlanStepStatusChanged { .. } => {}
                EffectEvent::ToolStreamChunk { .. } => {}
                // Session steps don't affect in-flight counts — they're informational.
                EffectEvent::ToolSessionStep { .. } => {}
            }
            if !entry.any() {
                counts.remove(&context_id);
            }
        }

        let subs = self.effect_subscribers.read().await;
        let cloned: Vec<Arc<dyn EffectSubscriber>> = subs.iter().cloned().collect();
        drop(subs);

        let is_llm_completed = matches!(event, EffectEvent::LlmCompleted { .. });
        if is_llm_completed {
            // Awaitable subscribers (e.g. `ProvenanceEffectSubscriber`) finish before
            // `emit` returns so causal-completeness contracts hold for downstream
            // reads such as `conversation_context`. Background subscribers run
            // detached: drift scoring (100–500 ms per call, ~2 s FastEmbed cold-start),
            // status updates, and SSE relays never gate the user-facing critical path
            // of an LLM hop.
            let mut awaitable: Vec<Arc<dyn EffectSubscriber>> = Vec::new();
            for sub in cloned {
                match sub.tier() {
                    EffectSubscriberTier::Awaitable => awaitable.push(sub),
                    EffectSubscriberTier::Background => {
                        let event_for_bg = event.clone();
                        tokio::spawn(async move {
                            if let Err(e) = sub.on_effect(&event_for_bg).await {
                                tracing::warn!(
                                    error = ?e,
                                    tier = "background",
                                    "Effect subscriber failed"
                                );
                            }
                        });
                    }
                }
            }
            let results = join_all(awaitable.into_iter().map(|sub| {
                let event = event.clone();
                async move { sub.on_effect(&event).await }
            }))
            .await;
            for res in results {
                if let Err(e) = res {
                    tracing::warn!(error = ?e, tier = "awaitable", "Effect subscriber failed");
                }
            }
        } else {
            for sub in cloned {
                if let Err(e) = sub.on_effect(&event).await {
                    tracing::warn!(error = ?e, "Effect subscriber failed");
                }
            }
        }
        Ok(())
    }
}

impl Default for BusWithEffects {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BusApi for BusWithEffects {
    async fn emit(&self, envelope: Envelope) -> crate::Result<()> {
        self.bus.emit(envelope).await
    }

    async fn subscribe(&self, subscriber: Arc<dyn Subscriber>) {
        self.bus.subscribe(subscriber).await;
    }

    async fn stream(&self) -> BusStream<Envelope> {
        self.bus.stream().await
    }
}

#[async_trait]
impl EffectEmitter for BusWithEffects {
    async fn emit(&self, event: EffectEvent) -> crate::Result<()> {
        self.process_effect(event.clone()).await?;
        self.bus
            .emit(Envelope::now(None, Payload::Effect(event)))
            .await
    }

    async fn subscribe_effect_subscriber(&self, subscriber: Arc<dyn EffectSubscriber>) {
        let mut subs = self.effect_subscribers.write().await;
        subs.push(subscriber);
    }
}

#[async_trait]
impl EffectLiveness for BusWithEffects {
    async fn in_flight(&self, context_id: &ContextId) -> InFlightCounts {
        let counts = self.counts.read().await;
        counts.get(context_id).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };

    use async_trait::async_trait;
    use serde_json::json;

    use super::{
        BusWithEffects, EffectEmitter, EffectEvent, EffectSubscriber, EffectSubscriberTier,
        LlmEffectMetadata, Outcome, ToolNameResolution,
    };
    use crate::ids::ContextId;

    struct DelayedFlagSubscriber {
        delay: Duration,
        flag: Arc<AtomicBool>,
        tier: EffectSubscriberTier,
    }

    impl DelayedFlagSubscriber {
        fn awaitable(delay: Duration, flag: Arc<AtomicBool>) -> Self {
            Self {
                delay,
                flag,
                tier: EffectSubscriberTier::Awaitable,
            }
        }

        fn background(delay: Duration, flag: Arc<AtomicBool>) -> Self {
            Self {
                delay,
                flag,
                tier: EffectSubscriberTier::Background,
            }
        }
    }

    #[async_trait]
    impl EffectSubscriber for DelayedFlagSubscriber {
        async fn on_effect(&self, _event: &EffectEvent) -> crate::Result<()> {
            tokio::time::sleep(self.delay).await;
            self.flag.store(true, Ordering::Relaxed);
            Ok(())
        }

        fn tier(&self) -> EffectSubscriberTier {
            self.tier
        }
    }

    fn llm_metadata() -> LlmEffectMetadata {
        LlmEffectMetadata {
            client: "test".to_string(),
            model: "test-model".to_string(),
            function_name: "TestFn".to_string(),
            prompt: json!(null),
            metadata: json!({}),
            tool_name: ToolNameResolution::NotApplicable,
        }
    }

    fn llm_completed_event() -> EffectEvent {
        EffectEvent::LlmCompleted {
            context_id: ContextId::new(0, 0),
            metadata: llm_metadata(),
            usage: None,
            result_payload: None,
            duration_ms: 0,
            outcome: Outcome::Success,
            rejection_reason: None,
        }
    }

    #[tokio::test]
    async fn llm_completed_awaits_awaitable_subscriber_before_returning() {
        let bus = BusWithEffects::new();
        let flag = Arc::new(AtomicBool::new(false));
        let subscriber = Arc::new(DelayedFlagSubscriber::awaitable(
            Duration::from_millis(50),
            Arc::clone(&flag),
        ));
        bus.subscribe_effect(subscriber).await;

        EffectEmitter::emit(&bus, llm_completed_event())
            .await
            .expect("emit should succeed");

        assert!(flag.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn llm_completed_does_not_await_background_subscriber() {
        let bus = BusWithEffects::new();
        let flag = Arc::new(AtomicBool::new(false));
        let subscriber_delay = Duration::from_millis(200);
        bus.subscribe_effect(Arc::new(DelayedFlagSubscriber::background(
            subscriber_delay,
            Arc::clone(&flag),
        )))
        .await;

        let started = Instant::now();
        EffectEmitter::emit(&bus, llm_completed_event())
            .await
            .expect("emit should succeed");
        let elapsed = started.elapsed();

        assert!(
            elapsed < subscriber_delay / 2,
            "Background subscriber blocked emit: elapsed {elapsed:?} ≥ half of {subscriber_delay:?}"
        );
        assert!(
            !flag.load(Ordering::Relaxed),
            "Background subscriber should still be running when emit returns"
        );

        tokio::time::sleep(subscriber_delay * 2).await;
        assert!(
            flag.load(Ordering::Relaxed),
            "Background subscriber must still execute after emit returns"
        );
    }

    #[tokio::test]
    async fn llm_completed_only_awaits_awaitable_when_mixed() {
        let bus = BusWithEffects::new();
        let awaitable_flag = Arc::new(AtomicBool::new(false));
        let background_flag = Arc::new(AtomicBool::new(false));
        let awaitable_delay = Duration::from_millis(50);
        let background_delay = Duration::from_millis(500);

        bus.subscribe_effect(Arc::new(DelayedFlagSubscriber::awaitable(
            awaitable_delay,
            Arc::clone(&awaitable_flag),
        )))
        .await;
        bus.subscribe_effect(Arc::new(DelayedFlagSubscriber::background(
            background_delay,
            Arc::clone(&background_flag),
        )))
        .await;

        let started = Instant::now();
        EffectEmitter::emit(&bus, llm_completed_event())
            .await
            .expect("emit should succeed");
        let elapsed = started.elapsed();

        assert!(awaitable_flag.load(Ordering::Relaxed));
        assert!(!background_flag.load(Ordering::Relaxed));
        assert!(
            elapsed < background_delay / 2,
            "Background subscriber dragged onto hot path: elapsed {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn non_llm_completed_awaits_subscriber_before_returning() {
        let bus = BusWithEffects::new();
        let flag = Arc::new(AtomicBool::new(false));
        let subscriber = Arc::new(DelayedFlagSubscriber::background(
            Duration::from_millis(50),
            Arc::clone(&flag),
        ));
        bus.subscribe_effect(subscriber).await;

        let event = EffectEvent::ToolStreamChunk {
            context_id: ContextId::new(0, 0),
            chunk: json!({"kind": "test"}),
        };
        EffectEmitter::emit(&bus, event)
            .await
            .expect("emit should succeed");

        assert!(flag.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn llm_completed_awaitable_subscribers_run_concurrently() {
        let bus = BusWithEffects::new();
        let flags: Vec<Arc<AtomicBool>> =
            (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
        let subscriber_delay = Duration::from_millis(200);
        for flag in &flags {
            bus.subscribe_effect(Arc::new(DelayedFlagSubscriber::awaitable(
                subscriber_delay,
                Arc::clone(flag),
            )))
            .await;
        }

        let started = Instant::now();
        EffectEmitter::emit(&bus, llm_completed_event())
            .await
            .expect("emit should succeed");
        let elapsed = started.elapsed();

        for (idx, flag) in flags.iter().enumerate() {
            assert!(
                flag.load(Ordering::Relaxed),
                "subscriber {idx} did not complete before emit returned"
            );
        }
        let upper_bound = subscriber_delay * 2;
        assert!(
            elapsed < upper_bound,
            "expected concurrent fan-out (~{subscriber_delay:?}), got {elapsed:?}"
        );
    }
}
