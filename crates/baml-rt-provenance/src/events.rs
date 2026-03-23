use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use baml_rt_core::{
    Citation, Outcome,
    bus::PlanningSupersessionKind,
    ids::{
        ActivityAnchorId, AgentId, ArtifactId, ContextId, IntentId, MessageId, PlanId, PlanStepId,
        TaskId,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, Value as JsonValue};

// Process-local monotonic counter for provenance event IDs.
//
// IDs are persisted in file-backed stores and reused as part of node identities
// (e.g. `tool_call:prov-<id>`). If every process started at `prov-1`, reopening
// the same DB would collide with old IDs and MERGE/ON CREATE would reuse stale
// nodes. Seed from wall-clock nanoseconds to make IDs unique across process
// restarts while remaining monotonic within a process.
static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn seed_event_counter() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let pid_component = (std::process::id() as u64) & 0xFFFF;
    nanos.saturating_add(pid_component).max(1)
}

fn next_activity_anchor_id() -> ActivityAnchorId {
    let mut current = EVENT_COUNTER.load(Ordering::Relaxed);
    if current == 0 {
        let seed = seed_event_counter();
        match EVENT_COUNTER.compare_exchange(0, seed, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => current = seed,
            Err(existing) => current = existing,
        }
    }
    let id = EVENT_COUNTER.fetch_add(1, Ordering::Relaxed).max(current);
    ActivityAnchorId::from_counter(id)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct AgentType(String);

impl AgentType {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return None;
        }
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LlmUsage {
    Known {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        cached_input_tokens: Option<u64>,
    },
    Unknown,
}

/// Per-citation similarity entry in the provenance record.
///
/// Stores both the scoring result **and** the resolved evidence so the API can
/// surface the actual text that the LLM cited — not just the shorthand ref.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmCitationSimilarity {
    /// The ref number (`N` in `#N` or `@N`).
    pub n: u32,
    /// `true` for history refs (`#N`), `false` for archive refs (`@N`).
    pub is_history: bool,
    /// Counter-evidence citation (`!#N` or `!@N`); excluded from `mean_similarity`.
    #[serde(default)]
    pub negated: bool,
    /// Cosine similarity between the decision text and the cited content.
    pub similarity: f32,
    /// Raw citation string exactly as the LLM emitted it (e.g. `"#1"`, `"@2:3-5"`, `"!@1"`).
    #[serde(default)]
    pub raw: String,
    /// Stable event ID of the cited activity — usable for provenance graph lookup.
    #[serde(default)]
    pub activity_anchor: String,
    /// First 400 characters of the resolved content of the cited entry.
    ///
    /// For `#N` history refs this is the message/tool-call text; for `@N` archive
    /// refs this is the archived tool result (scoped to any requested line range).
    #[serde(default)]
    pub content_preview: String,
}

/// Citation-grounded drift info stored on every LLM call completion that produced citations.
///
/// This is the persisted form of [`baml_rt_embedding::CitationDriftAssessment`]; it stores
/// both the scoring results and the resolved evidence text so downstream consumers (API,
/// UI, eval tooling) can display what the model actually cited without re-resolving.
///
/// ## Interpreting `mean_similarity`
///
/// Calibrated ranges (from `tests/fixtures/drift/`):
///
/// - `> 0.85` and `coverage > 0` — near-verbatim copy of archive; **synthesis BIPIA signature**
/// - `0.67–0.78` — legitimate synthesis: paraphrase + reorganise from retrieved data
/// - `0.40–0.67` — moderate grounding; same domain, partial overlap
/// - `< 0.40` — likely wrong archive cited, or very weak grounding
/// - `= 1.0` with `coverage = 0` — **vacuous**: no citations were emitted at all
///
/// ## Interpreting `coverage`
///
/// `coverage = 0` is the primary signal for *missing citations*. In
/// [`baml_rt_tools::citations::CitationMode::Enforce`] this causes the call to be
/// rejected at the source before it reaches this record.
///
/// ## BIPIA composite rule
///
/// Combine with `plan_drift.step_alignment` from the same call to evaluate the
/// 2D injection firewall. See [`baml_rt_embedding::score_bipia_signal`] for the
/// full rule and threshold guidance. Neither signal alone is sufficient:
/// - `step_alignment` alone misses synthesis injections (step descriptions too broad)
/// - `mean_similarity` alone fires on legitimate grounded synthesis (0.67–0.78)
/// - Together they isolate the injection quadrant reliably across 39 test scenarios
///
/// ## Known limitations (unchanged from the scoring layer)
///
/// Numeric hallucination and broad generalisation errors are **not detectable** via
/// this signal. Both "$7.8M revenue" and "$4.2M revenue" embed near the same
/// "Q3 revenue figure" centroid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmCitationDriftInfo {
    /// One entry per citation the LLM emitted, including negated counter-evidence.
    /// Negated entries are reported here but excluded from `mean_similarity`.
    pub per_citation: Vec<LlmCitationSimilarity>,
    /// Mean cosine similarity across **positive** (non-negated) citations only.
    /// `1.0` is vacuous when `coverage = 0` (no citations) or when all citations are negated.
    pub mean_similarity: f32,
    /// Fraction of decisions that emitted at least one citation (cited_decisions / total_decisions).
    /// `0.0` means no citations were provided — the primary missing-citation signal.
    pub coverage: f32,
    pub total_decisions: usize,
    pub cited_decisions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmDriftInfo {
    pub score: f32,
    pub severity: String,
    pub mode: String,
    pub warn_min_score: f32,
    pub block_min_score: f32,
    pub intent_text_preview: String,
    pub response_text_preview: String,
    /// The plan step description that the response was compared against.
    /// Present for PlanCommitted calls; empty for pre-plan calls.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub step_text_preview: String,
    /// Plan-anchored drift fields — present only when the task has an active
    /// committed plan at the time of the LLM call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_drift: Option<LlmPlanDriftInfo>,
    /// Citation-grounded drift — present when the LLM produced citations.
    /// Independent signal; composition with tactical/plan drift is empirical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_drift: Option<LlmCitationDriftInfo>,
}

/// Plan-anchored drift scores attached to an LLM call completion.
///
/// Discriminated by plan phase so the pre-plan/post-plan distinction is
/// preserved end-to-end from scorer → events → store → API → UI.
/// Pre-plan variants structurally cannot contain step alignment.
/// Post-plan variants structurally guarantee it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "phase")]
pub enum LlmPlanDriftInfo {
    #[serde(rename = "pre_plan")]
    PrePlan {
        intent_alignment: f32,
        trajectory_drift: f32,
        plan_adherence_score: f32,
        composite_severity: String,
    },
    #[serde(rename = "plan_committed")]
    PlanCommitted {
        intent_alignment: f32,
        step_alignment: f32,
        /// Cross-encoder relevance logit for (step_description, response).
        /// Always present when PlanCommitted — the reranker is always configured.
        cross_encoder_step_score: f32,
        trajectory_drift: f32,
        plan_adherence_score: f32,
        composite_severity: String,
    },
}

impl LlmPlanDriftInfo {
    pub fn intent_alignment(&self) -> f32 {
        match self {
            Self::PrePlan {
                intent_alignment, ..
            } => *intent_alignment,
            Self::PlanCommitted {
                intent_alignment, ..
            } => *intent_alignment,
        }
    }

    pub fn step_alignment(&self) -> Option<f32> {
        match self {
            Self::PrePlan { .. } => None,
            Self::PlanCommitted { step_alignment, .. } => Some(*step_alignment),
        }
    }

    pub fn trajectory_drift(&self) -> f32 {
        match self {
            Self::PrePlan {
                trajectory_drift, ..
            } => *trajectory_drift,
            Self::PlanCommitted {
                trajectory_drift, ..
            } => *trajectory_drift,
        }
    }

    pub fn plan_adherence_score(&self) -> f32 {
        match self {
            Self::PrePlan {
                plan_adherence_score,
                ..
            } => *plan_adherence_score,
            Self::PlanCommitted {
                plan_adherence_score,
                ..
            } => *plan_adherence_score,
        }
    }

    pub fn composite_severity(&self) -> &str {
        match self {
            Self::PrePlan {
                composite_severity, ..
            } => composite_severity,
            Self::PlanCommitted {
                composite_severity, ..
            } => composite_severity,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CallScope {
    Message { message_id: MessageId },
    Task { task_id: TaskId },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStepSpec {
    pub step_id: PlanStepId,
    pub description: String,
    pub order: u32,
    #[serde(default)]
    pub depends_on: Vec<PlanStepId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProvEventData {
    LlmCallStarted {
        scope: CallScope,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
    },
    LlmCallCompleted {
        scope: CallScope,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
        drift: Option<Box<LlmDriftInfo>>,
        /// Parsed citation strings co-produced by the LLM in this call.
        /// Empty when the BAML wrapper type produced no citations.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<String>,
    },
    ToolCallStarted {
        scope: CallScope,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: JsonValue,
        /// For system/internal_a2a: the delegated-to agent package (write-time provenance).
        /// Emitted on start so WAS_DELEGATED_TO exists during delegation (before completion).
        delegation_target: Option<String>,
    },
    ToolCallCompleted {
        scope: CallScope,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: JsonValue,
        duration_ms: u64,
        outcome: Outcome,
        /// For system/internal_a2a: the delegated-to agent package (write-time provenance).
        delegation_target: Option<String>,
    },
    /// A single step within a tool session (Open / SendDone / Read).
    /// Written synchronously so conversation_context sees session state mid-execution.
    ToolSessionStep {
        scope: CallScope,
        tool_name: String,
        session_id: String,
        /// Discriminant: "open" | "send_done" | "read".
        op_kind: String,
        /// For SendDone: full display string `"@1 tool 'summary' [N lines, KB]"`.
        header: Option<String>,
        /// For SendDone/Read: canonical archive ref string e.g. `"@1"`.
        archive_ref: Option<String>,
        /// For Read: grep pattern used.
        grep: Option<String>,
        /// For Read: line offset (0-based count of matched lines to skip).
        offset: Option<usize>,
        /// For Read: page limit used.
        limit: Option<usize>,
    },
    AgentBooted {
        agent_id: AgentId,
        agent_type: AgentType,
        agent_version: String,
        archive_path: String,
    },
    /// Existential only: task entity exists. Idempotent. No agent, no execution.
    TaskExists {
        task_id: TaskId,
        context_id: ContextId,
    },
    /// Agent begins executing the task. Deterministic: task_id, agent_id, context_id.
    TaskExecutionStarted {
        task_id: TaskId,
        agent_id: AgentId,
        context_id: ContextId,
    },
    /// Agent finishes (or abandons). May not occur.
    TaskExecutionEnded {
        task_id: TaskId,
        context_id: ContextId,
    },
    TaskStatusChanged {
        task_id: TaskId,
        old_status: Option<String>,
        new_status: Option<String>,
    },
    TaskArtifactGenerated {
        task_id: TaskId,
        artifact_id: Option<ArtifactId>,
        artifact_type: Option<String>,
    },
    IntentResolved {
        task_id: TaskId,
        intent_id: IntentId,
        description: String,
        /// Citation refs (`#N`, `@N`) for the history entries this intent was derived from.
        citations: Vec<Citation>,
        supersession: Option<PlanningSupersessionKind>,
        /// Cosine similarity between the previous intent embedding and this
        /// new one.  Present only on supersession events.
        ///
        /// - ≈ 1.0: new intent closely paraphrases the old one (legitimate replan)
        /// - ≈ 0.5: moderate semantic shift (suspicious)
        /// - ≈ 0.0: completely different goal (potential control hijacking)
        ///
        /// `None` for the first intent (no previous to compare against).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision_intent_drift: Option<f32>,
    },
    PlanGenerated {
        task_id: TaskId,
        intent_id: IntentId,
        plan_id: PlanId,
        steps: Vec<PlanStepSpec>,
        supersession: Option<PlanningSupersessionKind>,
    },
    PlanStepStatusChanged {
        task_id: TaskId,
        intent_id: IntentId,
        plan_id: PlanId,
        step_id: PlanStepId,
        old_status: Option<String>,
        new_status: String,
        citations: Vec<Citation>,
    },
    MessageReceived {
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        /// Agent that receives the message. Required: a message is always sent to an agent.
        agent_id: AgentId,
    },
    MessageSent {
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        /// Agent that sent the message. Required: a message is always sent from an agent.
        agent_id: AgentId,
    },
    /// Instantaneous event: output of an LLM prompt was rejected (e.g. ToolSessionPlan step missing op).
    /// Linked to the prompt entity via the LLM call completion event id.
    PromptRejected {
        scope: CallScope,
        llm_call_activity_anchor: ActivityAnchorId,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskScopedEvent {
    pub id: ActivityAnchorId,
    pub context_id: ContextId,
    pub task_id: TaskId,
    pub timestamp_ms: u64,
    pub data: ProvEventData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalEvent {
    pub id: ActivityAnchorId,
    pub context_id: ContextId,
    pub timestamp_ms: u64,
    pub data: ProvEventData,
}

/// AgentBooted has no context by design—it is a global event before any conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBootedEvent {
    pub id: ActivityAnchorId,
    pub timestamp_ms: u64,
    pub data: ProvEventData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProvEvent {
    Task(TaskScopedEvent),
    Global(GlobalEvent),
    /// Context-free: agent boot precedes any conversation.
    AgentBooted(AgentBootedEvent),
}

impl ProvEvent {
    pub fn id(&self) -> &ActivityAnchorId {
        match self {
            ProvEvent::Task(event) => &event.id,
            ProvEvent::Global(event) => &event.id,
            ProvEvent::AgentBooted(event) => &event.id,
        }
    }

    /// Context for scoped events. AgentBooted has no context by design.
    pub fn context_id_opt(&self) -> Option<&ContextId> {
        match self {
            ProvEvent::Task(event) => Some(&event.context_id),
            ProvEvent::Global(event) => Some(&event.context_id),
            ProvEvent::AgentBooted(_) => None,
        }
    }

    /// Panics if called on AgentBooted (which has no context).
    pub fn context_id(&self) -> &ContextId {
        self.context_id_opt()
            .expect("AgentBooted has no context; use context_id_opt()")
    }

    pub fn task_id(&self) -> Option<&TaskId> {
        match self {
            ProvEvent::Task(event) => Some(&event.task_id),
            ProvEvent::Global(_) | ProvEvent::AgentBooted(_) => None,
        }
    }

    pub fn timestamp_ms(&self) -> u64 {
        match self {
            ProvEvent::Task(event) => event.timestamp_ms,
            ProvEvent::Global(event) => event.timestamp_ms,
            ProvEvent::AgentBooted(event) => event.timestamp_ms,
        }
    }

    pub fn data(&self) -> &ProvEventData {
        match self {
            ProvEvent::Task(event) => &event.data,
            ProvEvent::Global(event) => &event.data,
            ProvEvent::AgentBooted(event) => &event.data,
        }
    }

    /// Agent for MessageReceived/MessageSent. A message is always sent to/from an agent.
    pub fn message_agent_id(&self) -> Option<&AgentId> {
        match self.data() {
            ProvEventData::MessageReceived { agent_id, .. }
            | ProvEventData::MessageSent { agent_id, .. } => Some(agent_id),
            _ => None,
        }
    }

    pub fn llm_call_started_global(
        context_id: ContextId,
        message_id: MessageId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallStarted {
                scope: CallScope::Message { message_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
            },
        })
    }

    pub fn llm_call_started_task(
        context_id: ContextId,
        task_id: TaskId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallStarted {
                scope: CallScope::Task { task_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn llm_call_completed_global(
        context_id: ContextId,
        message_id: MessageId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
    ) -> Self {
        Self::llm_call_completed_global_with_drift(
            context_id,
            message_id,
            client,
            model,
            function_name,
            prompt,
            metadata,
            usage,
            duration_ms,
            outcome,
            None,
            vec![],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn llm_call_completed_global_with_drift(
        context_id: ContextId,
        message_id: MessageId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
        drift: Option<Box<LlmDriftInfo>>,
        citations: Vec<String>,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message { message_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
                usage,
                duration_ms,
                outcome,
                drift,
                citations,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn llm_call_completed_task(
        context_id: ContextId,
        task_id: TaskId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
    ) -> Self {
        Self::llm_call_completed_task_with_drift(
            context_id,
            task_id,
            client,
            model,
            function_name,
            prompt,
            metadata,
            usage,
            duration_ms,
            outcome,
            None,
            vec![],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn llm_call_completed_task_with_drift(
        context_id: ContextId,
        task_id: TaskId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
        drift: Option<Box<LlmDriftInfo>>,
        citations: Vec<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Task { task_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
                usage,
                duration_ms,
                outcome,
                drift,
                citations,
            },
        })
    }

    /// Same as `llm_call_completed_task_with_drift` but also carries citation strings
    /// co-produced by the LLM call.
    #[allow(clippy::too_many_arguments)]
    pub fn llm_call_completed_task_with_citations(
        context_id: ContextId,
        task_id: TaskId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: JsonValue,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
        drift: Option<Box<LlmDriftInfo>>,
        citations: Vec<String>,
    ) -> Self {
        Self::llm_call_completed_task_with_drift(
            context_id,
            task_id,
            client,
            model,
            function_name,
            prompt,
            metadata,
            usage,
            duration_ms,
            outcome,
            drift,
            citations,
        )
    }

    pub fn prompt_rejected_global(
        context_id: ContextId,
        message_id: MessageId,
        llm_call_activity_anchor: ActivityAnchorId,
        reason: String,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::PromptRejected {
                scope: CallScope::Message { message_id },
                llm_call_activity_anchor,
                reason,
            },
        })
    }

    pub fn prompt_rejected_task(
        context_id: ContextId,
        task_id: TaskId,
        llm_call_activity_anchor: ActivityAnchorId,
        reason: String,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::PromptRejected {
                scope: CallScope::Task { task_id },
                llm_call_activity_anchor,
                reason,
            },
        })
    }

    pub fn tool_call_started_global(
        context_id: ContextId,
        message_id: MessageId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: JsonValue,
        delegation_target: Option<String>,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallStarted {
                scope: CallScope::Message { message_id },
                tool_name,
                function_name,
                args,
                metadata,
                delegation_target,
            },
        })
    }

    pub fn tool_call_started_task(
        context_id: ContextId,
        task_id: TaskId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: JsonValue,
        delegation_target: Option<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallStarted {
                scope: CallScope::Task { task_id },
                tool_name,
                function_name,
                args,
                metadata,
                delegation_target,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_call_completed_global(
        context_id: ContextId,
        message_id: MessageId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: JsonValue,
        duration_ms: u64,
        outcome: Outcome,
        delegation_target: Option<String>,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallCompleted {
                scope: CallScope::Message { message_id },
                tool_name,
                function_name,
                args,
                metadata,
                duration_ms,
                outcome,
                delegation_target,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_call_completed_task(
        context_id: ContextId,
        task_id: TaskId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: JsonValue,
        duration_ms: u64,
        outcome: Outcome,
        delegation_target: Option<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallCompleted {
                scope: CallScope::Task { task_id },
                tool_name,
                function_name,
                args,
                metadata,
                duration_ms,
                outcome,
                delegation_target,
            },
        })
    }

    pub fn tool_session_step(
        context_id: ContextId,
        scope: CallScope,
        tool_name: String,
        session_id: String,
        op: &crate::store::SessionStepOp,
    ) -> Self {
        let (op_kind, header, archive_ref, grep, offset, limit) = match op {
            crate::store::SessionStepOp::Open => ("open".to_string(), None, None, None, None, None),
            crate::store::SessionStepOp::SendDone {
                archive_ref,
                header,
            } => (
                "send_done".to_string(),
                Some(header.clone()),
                Some(archive_ref.clone()),
                None,
                None,
                None,
            ),
            crate::store::SessionStepOp::Read {
                archive_ref,
                grep,
                offset,
                limit,
            } => (
                "read".to_string(),
                None,
                Some(archive_ref.clone()),
                grep.clone(),
                Some(*offset),
                Some(*limit),
            ),
        };
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolSessionStep {
                scope,
                tool_name,
                session_id,
                op_kind,
                header,
                archive_ref,
                grep,
                offset,
                limit,
            },
        })
    }

    pub fn agent_booted(
        agent_id: AgentId,
        agent_type: AgentType,
        agent_version: String,
        archive_path: String,
    ) -> Self {
        ProvEvent::AgentBooted(AgentBootedEvent {
            id: next_activity_anchor_id(),
            timestamp_ms: now_millis(),
            data: ProvEventData::AgentBooted {
                agent_id,
                agent_type,
                agent_version,
                archive_path,
            },
        })
    }

    /// Existential only. Idempotent.
    pub fn task_exists(context_id: ContextId, task_id: TaskId) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskExists {
                task_id,
                context_id,
            },
        })
    }

    /// Agent begins executing. Emit after TaskExists.
    pub fn task_execution_started(
        context_id: ContextId,
        task_id: TaskId,
        agent_id: AgentId,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskExecutionStarted {
                task_id,
                agent_id,
                context_id,
            },
        })
    }

    /// Agent finishes. Emit when status becomes COMPLETED or terminal.
    pub fn task_execution_ended(context_id: ContextId, task_id: TaskId) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskExecutionEnded {
                task_id,
                context_id,
            },
        })
    }

    pub fn task_status_changed(
        context_id: ContextId,
        task_id: TaskId,
        old_status: Option<String>,
        new_status: Option<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskStatusChanged {
                task_id,
                old_status,
                new_status,
            },
        })
    }

    pub fn task_artifact_generated(
        context_id: ContextId,
        task_id: TaskId,
        artifact_id: Option<ArtifactId>,
        artifact_type: Option<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskArtifactGenerated {
                task_id,
                artifact_id,
                artifact_type,
            },
        })
    }

    pub fn intent_resolved(
        context_id: ContextId,
        task_id: TaskId,
        intent_id: impl Into<IntentId>,
        description: String,
        citations: Vec<Citation>,
        supersession: Option<PlanningSupersessionKind>,
        revision_intent_drift: Option<f32>,
    ) -> Self {
        let intent_id = intent_id.into();
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::IntentResolved {
                task_id,
                intent_id,
                description,
                citations,
                supersession,
                revision_intent_drift,
            },
        })
    }

    pub fn plan_generated(
        context_id: ContextId,
        task_id: TaskId,
        intent_id: impl Into<IntentId>,
        plan_id: impl Into<PlanId>,
        steps: Vec<PlanStepSpec>,
        supersession: Option<PlanningSupersessionKind>,
    ) -> Self {
        let intent_id = intent_id.into();
        let plan_id = plan_id.into();
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::PlanGenerated {
                task_id,
                intent_id,
                plan_id,
                steps,
                supersession,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn plan_step_status_changed(
        context_id: ContextId,
        task_id: TaskId,
        intent_id: impl Into<IntentId>,
        plan_id: impl Into<PlanId>,
        step_id: impl Into<PlanStepId>,
        old_status: Option<String>,
        new_status: String,
        citations: Vec<Citation>,
    ) -> Self {
        let intent_id = intent_id.into();
        let plan_id = plan_id.into();
        let step_id = step_id.into();
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::PlanStepStatusChanged {
                task_id,
                intent_id,
                plan_id,
                step_id,
                old_status,
                new_status,
                citations,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn message_received_task(
        context_id: ContextId,
        task_id: TaskId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        agent_id: AgentId,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id,
            timestamp_ms,
            data: ProvEventData::MessageReceived {
                id,
                role,
                content,
                metadata,
                agent_id,
            },
        })
    }

    pub fn message_received_global(
        context_id: ContextId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        agent_id: AgentId,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms,
            data: ProvEventData::MessageReceived {
                id,
                role,
                content,
                metadata,
                agent_id,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn message_sent_task(
        context_id: ContextId,
        task_id: TaskId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        agent_id: AgentId,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_activity_anchor_id(),
            context_id,
            task_id,
            timestamp_ms,
            data: ProvEventData::MessageSent {
                id,
                role,
                content,
                metadata,
                agent_id,
            },
        })
    }

    pub fn message_sent_global(
        context_id: ContextId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        agent_id: AgentId,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id,
            timestamp_ms,
            data: ProvEventData::MessageSent {
                id,
                role,
                content,
                metadata,
                agent_id,
            },
        })
    }
}
