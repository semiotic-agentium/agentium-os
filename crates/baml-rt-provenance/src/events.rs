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
use baml_rt_embedding::{BipiaSignalInputs, DriftMode, DriftSeverity};
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

/// Reserved metadata key: host allocates this anchor, then [`ProvEventData::ToolCallCompleted`] is written with the same id so `SendDone` can link via [`WAS_INFORMED_BY`](crate::vocabulary::semantic_labels::WAS_INFORMED_BY).
pub const BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR: &str =
    "baml_prov_reserved_tool_completion_anchor";

/// A freshly-allocated [`ActivityAnchorId`] that **must** be consumed by passing it
/// to a `*_with_id` [`ProvEvent`] constructor.
///
/// ## Why this type exists
///
/// The `event_order` counter that ends up in the provenance graph is assigned at
/// [`ProvEvent`] construction time. When two tokio tasks race (e.g. the QuickJS
/// task emitting `WORKING` and the drain-loop task emitting `COMPLETED`), the one
/// whose internal DB await finishes first will construct its event first and claim
/// the lower counter — corrupting logical ordering.
///
/// The fix: reserve the counter **before** any `async` await by calling
/// [`ReservedAnchor::allocate()`], then pass `self` to the `*_with_id` constructor
/// **after** the await. `#[must_use]` ensures the compiler warns if the allocation
/// is discarded without being consumed.
///
/// ## What this type does NOT protect against
///
/// Passing a `ReservedAnchor` originally intended for a tool-completion into
/// [`ProvEvent::task_status_changed_with_id`] is still technically possible — the
/// type erases to `ActivityAnchorId` on consumption. Preventing that would require
/// phantom-typed generics, which is not worth the complexity here.
///
/// ## Usage
///
/// ```rust,ignore
/// // BEFORE the await — reserve the ordering slot
/// let anchor = ReservedAnchor::allocate();
/// let out = self.inner.record_status_update(…).await?;
/// // AFTER the await — consume the anchor when building the event
/// let event = ProvEvent::task_status_changed_with_id(anchor, …);
/// ```
#[must_use = "ReservedAnchor must be passed to a ProvEvent *_with_id constructor; dropping it wastes a counter slot and leaves the ordering invariant unmet"]
pub struct ReservedAnchor(ActivityAnchorId);

impl ReservedAnchor {
    /// Reserve an activity anchor counter slot. Call this **before** any `async` await
    /// that precedes the event construction this anchor will be used for.
    pub fn allocate() -> Self {
        Self(next_activity_anchor_id())
    }

    /// Consume the anchor and return the underlying id for use in a `*_with_id` constructor.
    pub fn into_id(self) -> ActivityAnchorId {
        self.0
    }

    /// Borrow the raw anchor string (e.g. to write it into metadata for cross-event linking).
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Debug for ReservedAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ReservedAnchor").field(&self.0).finish()
    }
}

impl std::fmt::Display for ReservedAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<ReservedAnchor> for ActivityAnchorId {
    fn from(r: ReservedAnchor) -> Self {
        r.0
    }
}

/// Allocate an [`ActivityAnchorId`] counter slot before an async boundary.
///
/// Prefer [`ReservedAnchor::allocate()`] for new call sites — it is `#[must_use]`
/// and documents the temporal contract at the type level.
///
/// This free function is kept for call sites that immediately pass the id into a
/// `*_with_id` constructor without storing it in a variable.
#[must_use]
pub fn allocate_activity_anchor() -> ActivityAnchorId {
    next_activity_anchor_id()
}

/// Compatibility alias — use [`ReservedAnchor::allocate`] for new code.
#[must_use]
#[doc(hidden)]
pub fn allocate_tool_invocation_activity_anchor() -> ActivityAnchorId {
    allocate_activity_anchor()
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

/// Pre-resolved citation target: resolved at event emission time by the effect subscriber
/// using the live RefTable and conversation context. Stored on `LlmCallCompleted` so the
/// normalizer can emit CITED edges without needing the ephemeral `@N`/`#N` ref table.
///
/// The `target_node_id` is the **write-time node ID** constructed by the effect subscriber
/// using the same conventions the normalizer uses to create nodes (e.g. `"session-step:{anchor}"`
/// for SessionStep entities, `"message:{ctx}:{msg_id}"` for Message entities). This is a
/// write-time cross-reference, not a read-time heuristic — the ID is constructed once at
/// event emission and consumed by the normalizer to create the CITED edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedCitationTarget {
    /// Write-time node ID of the cited entity, constructed using normalizer conventions.
    /// For history refs (`#N`): `"message:{context_id}:{message_id}"`.
    /// For archive refs (`@N`): `"session-step:{activity_anchor}"` of the SendDone.
    pub target_node_id: String,
    /// Original citation string exactly as the model emitted it (`"#7"`, `"@8"`, `"@4:2-5"`).
    /// Stored as the `raw` attribute on the CITED graph edge so graph traversal consumers
    /// can reconstruct the citation without re-resolving ref numbers.
    #[serde(default)]
    pub raw: String,
    /// Line range qualification (for `@N:L1-L2`).
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
    /// Counter-evidence (`!@N`, `!#N`).
    pub negated: bool,
    /// Cosine similarity from drift scoring, if available.
    pub similarity: Option<f32>,
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
    pub severity: DriftSeverity,
    pub mode: DriftMode,
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

/// Shared numeric scores common to both plan phases.
/// Serialised with `#[serde(flatten)]` so the JSON wire shape stays flat:
/// `{ "intentAlignment": 0.3, "trajectoryDrift": 0.9, ... }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanDriftScores {
    pub intent_alignment: f32,
    pub trajectory_drift: f32,
    pub plan_adherence_score: f32,
    pub composite_severity: DriftSeverity,
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
        #[serde(flatten)]
        scores: PlanDriftScores,
    },
    #[serde(rename = "plan_committed")]
    PlanCommitted {
        #[serde(flatten)]
        scores: PlanDriftScores,
        step_alignment: f32,
        /// Cross-encoder relevance logit for (step_description, response).
        /// Always present when PlanCommitted — the reranker is always configured.
        cross_encoder_step_score: f32,
    },
}

impl LlmPlanDriftInfo {
    pub fn intent_alignment(&self) -> f32 {
        match self {
            Self::PrePlan { scores } | Self::PlanCommitted { scores, .. } => {
                scores.intent_alignment
            }
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
            Self::PrePlan { scores } | Self::PlanCommitted { scores, .. } => {
                scores.trajectory_drift
            }
        }
    }

    pub fn plan_adherence_score(&self) -> f32 {
        match self {
            Self::PrePlan { scores } | Self::PlanCommitted { scores, .. } => {
                scores.plan_adherence_score
            }
        }
    }

    pub fn composite_severity(&self) -> DriftSeverity {
        match self {
            Self::PrePlan { scores } | Self::PlanCommitted { scores, .. } => {
                scores.composite_severity
            }
        }
    }

    /// Return a copy of this info with `composite_severity` escalated.
    /// Used by the BIPIA firewall to escalate to `Block` when the 2D geometric
    /// fingerprint fires (low step_alignment + high cite_mean), even if individual
    /// 1D thresholds would produce only `Warn`.
    pub fn with_escalated_severity(mut self, severity: DriftSeverity) -> Self {
        match &mut self {
            Self::PlanCommitted { scores, .. } | Self::PrePlan { scores } => {
                scores.composite_severity = severity;
            }
        }
        self
    }
}

impl BipiaSignalInputs for LlmCitationDriftInfo {
    fn mean_similarity(&self) -> f32 {
        self.mean_similarity
    }
    fn positive_citation_count(&self) -> usize {
        self.per_citation.iter().filter(|c| !c.negated).count()
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
        /// Pre-resolved citation targets resolved at event emission time using the live RefTable.
        /// The normalizer uses these to emit CITED graph edges without the ephemeral ref table.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resolved_citations: Vec<ResolvedCitationTarget>,
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
        /// For SendDone: [`ActivityAnchorId`] of the `ToolCallCompleted` whose `tool_result` backs this `@N` row.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        informed_by_tool_activity_anchor: Option<String>,
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
        /// Validated citation refs produced by the model in this message (`#N`, `@N`, …).
        /// Written as CITED graph edges by the normalizer; not stored as a flat node attribute.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
    },
    MessageSent {
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        /// Agent that sent the message. Required: a message is always sent from an agent.
        agent_id: AgentId,
        /// Validated citation refs produced by the model in this message (`#N`, `@N`, …).
        /// Written as CITED graph edges by the normalizer; not stored as a flat node attribute.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
    },
    /// Instantaneous event: output of an LLM prompt was rejected (e.g. ToolSessionPlan step missing op).
    /// Linked to the prompt entity via the LLM call completion event id.
    PromptRejected {
        scope: CallScope,
        llm_call_activity_anchor: ActivityAnchorId,
        reason: String,
    },
    /// Detached `system/callback`: minted dispatch task/context was scheduled from a parent A2A turn.
    ///
    /// The runner emits this after an accepted [`AgentDispatchRequest`](baml_rt_core::AgentDispatchRequest)
    /// when dispatch `context_id`/`task_id` differ from the scheduling scope carried in request
    /// metadata (`schedulingContextId` / `schedulingTaskId`; see
    /// [`DISPATCH_METADATA_SCHEDULING_CONTEXT_ID`](baml_rt_core::DISPATCH_METADATA_SCHEDULING_CONTEXT_ID)).
    /// The normalizer records [`WAS_SCHEDULED_FROM`](crate::vocabulary::semantic_labels::WAS_SCHEDULED_FROM).
    CallbackDispatchContextsLinked {
        scheduling_context_id: ContextId,
        scheduling_task_id: TaskId,
        dispatch_context_id: ContextId,
        dispatch_task_id: TaskId,
        agent_id: AgentId,
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
        resolved_citations: Vec<ResolvedCitationTarget>,
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
                resolved_citations,
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
        resolved_citations: Vec<ResolvedCitationTarget>,
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
                resolved_citations,
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
            vec![],
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
        Self::tool_call_completed_global_with_id(
            next_activity_anchor_id(),
            context_id,
            message_id,
            tool_name,
            function_name,
            args,
            metadata,
            duration_ms,
            outcome,
            delegation_target,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_call_completed_global_with_id(
        id: impl Into<ActivityAnchorId>,
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
            id: id.into(),
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
        Self::tool_call_completed_task_with_id(
            next_activity_anchor_id(),
            context_id,
            task_id,
            tool_name,
            function_name,
            args,
            metadata,
            duration_ms,
            outcome,
            delegation_target,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_call_completed_task_with_id(
        id: impl Into<ActivityAnchorId>,
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
            id: id.into(),
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
                ..
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
        let informed_by = match op {
            crate::store::SessionStepOp::SendDone { informed_by, .. } => Some(informed_by.clone()),
            _ => None,
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
                informed_by_tool_activity_anchor: informed_by,
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
        Self::task_status_changed_with_id(
            next_activity_anchor_id(),
            context_id,
            task_id,
            old_status,
            new_status,
        )
    }

    /// Construct a [`ProvEvent::TaskStatusChanged`] using a pre-allocated anchor.
    ///
    /// Pass a [`ReservedAnchor`] (preferred — `#[must_use]` enforces pre-allocation) or a
    /// raw [`ActivityAnchorId`] (for deserialized anchors). The anchor must have been
    /// reserved **before** any `async` await separating this call from the logical emission
    /// point.
    pub fn task_status_changed_with_id(
        id: impl Into<ActivityAnchorId>,
        context_id: ContextId,
        task_id: TaskId,
        old_status: Option<String>,
        new_status: Option<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: id.into(),
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
                citations: Vec::new(),
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
                citations: Vec::new(),
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
        citations: Vec<Citation>,
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
                citations,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn message_sent_global(
        context_id: ContextId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        agent_id: AgentId,
        timestamp_ms: u64,
        citations: Vec<Citation>,
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
                citations,
            },
        })
    }

    /// Link a minted callback dispatch task to the scheduling A2A task (detached continuation).
    pub fn callback_dispatch_contexts_linked(
        dispatch_context_id: ContextId,
        scheduling_context_id: ContextId,
        scheduling_task_id: TaskId,
        dispatch_task_id: TaskId,
        agent_id: AgentId,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_activity_anchor_id(),
            context_id: dispatch_context_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::CallbackDispatchContextsLinked {
                scheduling_context_id,
                scheduling_task_id,
                dispatch_context_id,
                dispatch_task_id,
                agent_id,
            },
        })
    }
}
