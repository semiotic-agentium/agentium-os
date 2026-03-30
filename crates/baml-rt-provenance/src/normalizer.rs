use std::{
    collections::{HashMap, VecDeque},
    fmt,
};

use baml_rt_core::{
    BamlFunctionId,
    bus::PlanningSupersessionKind,
    ids::{
        ActivityAnchorId, AgentId, ArtifactId, ContextId, ExternalId, IntentId, MessageId, PlanId,
        PlanStepId, ProvVocabularyType, TaskId, UuidId,
    },
};
use serde_json::Value;

use crate::{
    document::ProvDocument,
    error::{ProvenanceError, Result},
    events::{CallScope, ProvEvent, ProvEventData},
    id_semantics::{
        AgentBootActivityId, AgentBootActivityInput, AgentRuntimeInstanceId,
        AgentRuntimeInstanceInput, ArchiveEntityId, ArchiveEntityInput,
        ArtifactByActivityAnchorEntityId, ArtifactByActivityAnchorEntityInput,
        ArtifactByIdEntityId, ArtifactByIdEntityInput, ArtifactByTypeEntityId,
        ArtifactByTypeEntityInput, ArtifactIdentity, DelegationTargetEntityId,
        DelegationTargetEntityInput, FailureClassificationActivityId,
        FailureClassificationActivityInput, FailureClassificationEntityId,
        FailureClassificationEntityInput, IntentEntityId, IntentEntityInput, LlmCallActivityId,
        LlmCallActivityInput, LlmPromptEntityId, LlmPromptEntityInput, MessageEntityId,
        MessageEntityInput, MessageProcessingActivityId, MessageProcessingActivityInput,
        PlanEntityId, PlanEntityInput, PlanStepEntityId, PlanStepEntityInput,
        PromptRejectedActivityId, PromptRejectedActivityInput, RunnerRuntimeInstanceId,
        SessionStepEntityId, SessionStepEntityInput, TaskEntityId, TaskEntityInput,
        TaskExecutionActivityId, TaskExecutionActivityInput, TaskStateEntityId,
        TaskStateEntityInput, ToolArgsEntityId, ToolArgsEntityInput, ToolCallActivityId,
        ToolCallActivityInput,
    },
    types::{
        Activity, Agent, Entity, ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef,
        QualifiedGeneration, Used, WasAssociatedWith, WasDerivedFrom, WasGeneratedBy,
    },
    vocabulary::{
        a2a, a2a_relation_types, a2a_relations, a2a_roles, agent_types, message_directions, prov,
        prov_roles,
    },
};

#[derive(Debug, Clone)]
pub struct NormalizedProv {
    pub document: ProvDocument,
    pub derived_relations: Vec<A2aDerivedRelation>,
    pub agent_labels: HashMap<String, String>,
}

fn parse_agent_id(event: &ProvEvent, raw: &str) -> Result<AgentId> {
    UuidId::parse_str(raw)
        .map(AgentId::from_uuid)
        .map_err(|_| ProvenanceError::InvalidEvent {
            activity_anchor: event.id().as_str().to_string(),
            reason: format!("invalid agent_id '{}' (expected UUID)", raw),
        })
}

fn canonical_message_role(event: &ProvEvent, role: &str) -> Result<&'static str> {
    let normalized = role.trim();
    if normalized.is_empty() {
        return Err(ProvenanceError::InvalidEvent {
            activity_anchor: event.id().as_str().to_string(),
            reason: "message role must be non-empty".to_string(),
        });
    }
    if normalized.eq_ignore_ascii_case("ROLE_USER") || normalized.eq_ignore_ascii_case("user") {
        return Ok("ROLE_USER");
    }
    if normalized.eq_ignore_ascii_case("ROLE_AGENT")
        || normalized.eq_ignore_ascii_case("agent")
        || normalized.eq_ignore_ascii_case("assistant")
        || normalized.eq_ignore_ascii_case("ROLE_ASSISTANT")
    {
        return Ok("ROLE_AGENT");
    }
    Err(ProvenanceError::InvalidEvent {
        activity_anchor: event.id().as_str().to_string(),
        reason: format!("invalid message role '{normalized}'"),
    })
}

fn payload_id_for_event(event: &ProvEvent, payload_kind: &str) -> String {
    format!("payload:{}:{}", event.id().as_str(), payload_kind)
}

/// Prior state from the backing store, used so message events can attach
/// (MessageProcessing, WAS_EXECUTED_BY, task_agent) when the task entity
/// was persisted by an earlier event (TaskExists + TaskExecutionStarted).
#[derive(Debug, Clone, Default)]
pub struct NormalizeContext {
    /// Agent id stored on the task entity (from TaskExecutionStarted). Used when normalizing
    /// MessageReceived/MessageSent so we emit WAS_EXECUTED_BY to the task's agent.
    pub task_agent_id: Option<AgentId>,
}

pub trait ProvNormalizer: Send + Sync {
    fn normalize(&self, event: &ProvEvent) -> Result<NormalizedProv> {
        self.normalize_with_context(event, None)
    }

    fn normalize_with_context(
        &self,
        event: &ProvEvent,
        context: Option<&NormalizeContext>,
    ) -> Result<NormalizedProv>;
}

/// Typed composite key for call-scope deduplication: `context_id:scope_id:agent_id`.
///
/// `scope_id` is the message_id (Message scope) or task_id (Task scope).
/// Used as the HashMap key in [`CallOrdinalState`] to track per-scope LLM/tool ordinal stacks.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ScopeKey {
    ctx: String,
    scope: String,
    agent: String,
}

impl fmt::Display for ScopeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.ctx, self.scope, self.agent)
    }
}

/// FIFO queue of ordinals opened by `*Started` and consumed by matching `*Completed` events.
#[derive(Debug, Default, Clone)]
pub(crate) struct OrdinalStack {
    /// Ordinals for in-flight Started calls (same scope, same channel), in completion order.
    open: VecDeque<u64>,
    /// Next ordinal to assign for a new Started or for a Completed-without-Started (orphan).
    next: u64,
}

/// LLM vs tool ordinal lanes: both use the same [`ScopeKey`] but must not share ordinals.
#[derive(Debug, Clone, Copy)]
enum CallOrdinalChannel {
    Llm,
    Tool,
}

/// State for deriving call ordinals from event stream order.
/// Normalizer-internal write-time state for tracking call ordinals and supersession edges.
///
/// This state exists only during the normalization phase (write path). It must **not** leak to
/// read paths — read paths reconstruct ordering from graph edges and the persisted
/// `event_order` property.
#[derive(Debug, Default)]
pub(crate) struct CallOrdinalState {
    /// Per-scope LLM call ordinals (paired Started/Completed, or monotonic orphans).
    pub(crate) llm_ordinals: HashMap<ScopeKey, OrdinalStack>,
    /// Per-scope tool call ordinals (paired Started/Completed, or monotonic orphans).
    pub(crate) tool_ordinals: HashMap<ScopeKey, OrdinalStack>,
    /// activity_anchor.as_str() -> (scope_key, ordinal) for completed LLM calls; used by PromptRejected.
    pub(crate) llm_event_to_scope_ordinal: HashMap<String, (ScopeKey, u64)>,
    /// `ToolCallCompleted` event anchor -> derived ToolCall activity id (session SendDone influence edges).
    pub(crate) tool_call_anchor_to_activity: HashMap<String, ProvActivityId>,
    /// task_id.as_str() -> latest canonical intent entity id.
    pub(crate) latest_intent_entity_by_task: HashMap<String, ProvEntityId>,
    /// task_id.as_str() -> latest canonical plan entity id.
    pub(crate) latest_plan_entity_by_task: HashMap<String, ProvEntityId>,
}

#[derive(Debug, Default)]
pub struct DefaultProvNormalizer {
    agent_registry: std::sync::Mutex<std::collections::HashSet<String>>,
    call_ordinal_state: std::sync::Mutex<CallOrdinalState>,
}

impl ProvNormalizer for DefaultProvNormalizer {
    fn normalize_with_context(
        &self,
        event: &ProvEvent,
        context: Option<&NormalizeContext>,
    ) -> Result<NormalizedProv> {
        let mut registry = self.agent_registry.lock().expect("agent registry lock");
        let mut call_state = self
            .call_ordinal_state
            .lock()
            .expect("call ordinal state lock");
        normalize_event_with_registry(event, &mut registry, &mut call_state, context)
    }
}

#[derive(Debug, Clone)]
pub struct A2aDerivedRelation {
    pub relation: A2aRelationType,
    pub from: ProvNodeRef,
    pub to: ProvNodeRef,
    pub attributes: HashMap<String, Value>,
}

#[derive(Debug, Clone, Copy)]
pub enum A2aRelationType {
    TaskHasMessage,
    TaskHasSessionStep,
    TaskHasArtifact,
    TaskCall,
    TaskStatusTransition,
    MessageCall,
    IntentReplacedBy,
    IntentRefinedBy,
    PlanReplacedBy,
    PlanRefinedBy,
    /// Citation from archive / observation (`@N`); maps to `WAS_INFORMED_BY` when persisted as a derived edge.
    InformedByObservation,
    /// `SendDone` session step informed by the tool invocation that produced the archived payload.
    InformedByToolInvocation,
    /// LLM decision cited a specific evidence source (`#N` history or `@N` archive).
    CitedSource,
    HasIntent,
    HasPlan,
}

impl A2aRelationType {
    /// The DB `rel_type` string exactly as written to `prov_edge` by the write batch.
    ///
    /// For variants whose write-batch arm is special-cased (CitedSource, HasIntent, HasPlan,
    /// IntentReplacedBy/Refined, PlanReplacedBy/Refined, InformedByToolInvocation), this
    /// returns the `semantic_labels` value that is actually stored. The variants that are
    /// handled by the write-batch's dynamic fall-through arm return the `a2a_relations` value
    /// which doubles as their canonical rel_type string.
    pub fn as_str(&self) -> &'static str {
        use crate::vocabulary::semantic_labels;
        match self {
            // Dynamic arm: write batch calls `relation.relation.as_str()` — these strings ARE the DB rel_type.
            A2aRelationType::TaskHasMessage => a2a_relations::TASK_MESSAGE,
            A2aRelationType::TaskHasSessionStep => a2a_relations::TASK_SESSION_STEP,
            A2aRelationType::TaskHasArtifact => a2a_relations::TASK_ARTIFACT,
            A2aRelationType::TaskCall => a2a_relations::TASK_CALL,
            A2aRelationType::TaskStatusTransition => a2a_relations::TASK_STATUS_TRANSITION,
            A2aRelationType::MessageCall => a2a_relations::MESSAGE_CALL,
            // Reserved variant — never emitted; a2a_relation_types prov_type is used instead.
            A2aRelationType::InformedByObservation => a2a_relations::INFORMED_BY_OBSERVATION,
            // Special-cased in write batch — return the semantic_labels value that IS stored.
            A2aRelationType::IntentReplacedBy => semantic_labels::WAS_REPLACED_BY,
            A2aRelationType::IntentRefinedBy => semantic_labels::WAS_REFINED_BY,
            A2aRelationType::PlanReplacedBy => semantic_labels::WAS_REPLACED_BY,
            A2aRelationType::PlanRefinedBy => semantic_labels::WAS_REFINED_BY,
            A2aRelationType::InformedByToolInvocation => semantic_labels::WAS_INFORMED_BY,
            A2aRelationType::CitedSource => semantic_labels::CITED,
            A2aRelationType::HasIntent => semantic_labels::HAS_INTENT,
            A2aRelationType::HasPlan => semantic_labels::HAS_PLAN,
        }
    }
}

fn prov_type<S: ProvVocabularyType>() -> String {
    S::VOCAB_TYPE.to_string()
}

#[derive(Debug, Clone)]
struct FailureResolution {
    class: String,
    evidence: String,
    code: Option<String>,
}

struct FailureClassificationTarget {
    call_kind: &'static str,
    scope_key: ScopeKey,
    ordinal: u64,
    failed_activity_id: ProvActivityId,
    evidence_entity: Option<ProvEntityId>,
}

fn is_unknown_placeholder(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unknown")
}

fn value_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim)
}

fn metadata_string_field(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn metadata_json_field(metadata: &Value, key: &str) -> Option<Value> {
    let value = metadata.get(key)?.clone();
    match &value {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        Value::Array(items) if items.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        _ => Some(value),
    }
}

/// Extract `agent_id` from untyped metadata JSON.
///
/// Centralises the `metadata.get("agent_id").and_then(Value::as_str)` pattern so that
/// the field name and extraction logic live in exactly one place.
fn extract_agent_id(metadata: &Value) -> Option<&str> {
    metadata.get("agent_id").and_then(Value::as_str)
}

fn model_from_value(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(model) = map
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !is_unknown_placeholder(s))
            {
                return Some(model.to_string());
            }
            for nested in map.values() {
                if let Some(model) = model_from_value(nested) {
                    return Some(model);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(model_from_value),
        _ => None,
    }
}

fn canonicalize_llm_client_model(
    event: &ProvEvent,
    client: &str,
    model: &str,
    prompt: &Value,
    metadata: &Value,
) -> Result<(String, String)> {
    let resolved_client = if is_unknown_placeholder(client) {
        value_str(metadata.get("client"))
            .filter(|v| !is_unknown_placeholder(v))
            .map(ToString::to_string)
            .or_else(|| {
                value_str(metadata.get("provider"))
                    .filter(|v| !is_unknown_placeholder(v))
                    .map(ToString::to_string)
            })
            .ok_or_else(|| ProvenanceError::MissingField {
                activity_anchor: event.id().as_str().to_string(),
                field: "a2a:client (BAML provider type)".to_string(),
            })?
    } else {
        client.trim().to_string()
    };

    let resolved_model = if is_unknown_placeholder(model) {
        model_from_value(prompt)
            .or_else(|| {
                value_str(metadata.get("model"))
                    .filter(|v| !is_unknown_placeholder(v))
                    .map(ToString::to_string)
            })
            .ok_or_else(|| ProvenanceError::MissingField {
                activity_anchor: event.id().as_str().to_string(),
                field: "a2a:model".to_string(),
            })?
    } else {
        model.trim().to_string()
    };

    Ok((resolved_client, resolved_model))
}

fn classify_failure_from_metadata(metadata: &Value) -> FailureResolution {
    let reason = metadata
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if reason.contains("reject") || reason.contains("policy") {
        return FailureResolution {
            class: "prompt_rejected".to_string(),
            evidence: "metadata".to_string(),
            code: None,
        };
    }

    let error = metadata
        .get("error")
        .map(Value::to_string)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if error.contains("unauthorized")
        || error.contains("authentication")
        || error.contains("api key")
        || error.contains("invalid_api_key")
        || error.contains("401")
    {
        return FailureResolution {
            class: "auth_error".to_string(),
            evidence: "metadata".to_string(),
            code: Some("auth".to_string()),
        };
    }
    if error.contains("timeout") {
        return FailureResolution {
            class: "timeout".to_string(),
            evidence: "metadata".to_string(),
            code: Some("timeout".to_string()),
        };
    }
    if error.contains("transport") || error.contains("network") || error.contains("http") {
        return FailureResolution {
            class: "transport_error".to_string(),
            evidence: "metadata".to_string(),
            code: Some("transport".to_string()),
        };
    }
    if error.contains("tool") {
        return FailureResolution {
            class: "tool_error".to_string(),
            evidence: "metadata".to_string(),
            code: Some("tool".to_string()),
        };
    }
    if error.contains("llm") || error.contains("model") {
        return FailureResolution {
            class: "llm_error".to_string(),
            evidence: "metadata".to_string(),
            code: Some("llm".to_string()),
        };
    }
    if metadata.get("status").is_some()
        || metadata.get("status_code").is_some()
        || metadata.get("url").is_some()
    {
        return FailureResolution {
            class: "provider_error".to_string(),
            evidence: "metadata".to_string(),
            code: Some("provider".to_string()),
        };
    }
    FailureResolution {
        class: "failed_graph_incomplete".to_string(),
        evidence: "metadata".to_string(),
        code: Some("incomplete".to_string()),
    }
}

/// Build deterministic scope key from operational identifiers: context_id:scope_id:agent_id.
/// Scope_id is message_id (Message scope) or task_id (Task scope).
fn build_call_scope_key(
    event: &ProvEvent,
    scope: &CallScope,
    metadata: &Value,
) -> Result<ScopeKey> {
    let context_id = event.context_id().as_str().to_string();
    let agent_id = extract_agent_id(metadata)
        .ok_or_else(|| ProvenanceError::MissingField {
            activity_anchor: event.id().as_str().to_string(),
            field: "metadata.agent_id".to_string(),
        })?
        .to_string();
    let scope_id = match scope {
        CallScope::Message { message_id } => message_id.as_str().to_string(),
        CallScope::Task { task_id } => {
            let tid = event
                .task_id()
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason: "task-scoped call requires task_id on event".to_string(),
                })?;
            if tid != task_id {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason: "scope task_id does not match event task_id".to_string(),
                });
            }
            task_id.as_str().to_string()
        }
    };
    Ok(ScopeKey {
        ctx: context_id,
        scope: scope_id,
        agent: agent_id,
    })
}

/// Assign ordinal for `*Started` / `*Completed` within one channel (`llm` vs `tool`) and scope.
///
/// - Each **Started** allocates `next`, pushes it onto `open`, and returns it.
/// - Each **Completed** pops the front of `open` (FIFO — first started completes first).
/// - **Orphan Completed** (no prior Started in this scope for this channel): allocate `next`
///   and advance it so consecutive completed-only events do not collapse to the same derived
///   activity id (which would drop failure-classification edges and corrupt ops rows).
fn get_ordinal_for_call(
    call_state: &mut CallOrdinalState,
    scope_key: &ScopeKey,
    is_started: bool,
    channel: CallOrdinalChannel,
) -> u64 {
    let map = match channel {
        CallOrdinalChannel::Llm => &mut call_state.llm_ordinals,
        CallOrdinalChannel::Tool => &mut call_state.tool_ordinals,
    };
    let st = map.entry(scope_key.clone()).or_default();
    if is_started {
        let o = st.next;
        st.next += 1;
        st.open.push_back(o);
        o
    } else {
        st.open.pop_front().unwrap_or_else(|| {
            let o = st.next;
            st.next += 1;
            o
        })
    }
}

pub fn normalize_event(event: &ProvEvent) -> Result<NormalizedProv> {
    normalize_event_with_registry(
        event,
        &mut std::collections::HashSet::new(),
        &mut CallOrdinalState::default(),
        None,
    )
}

fn normalize_event_with_registry(
    event: &ProvEvent,
    agent_registry: &mut std::collections::HashSet<String>,
    call_state: &mut CallOrdinalState,
    context: Option<&NormalizeContext>,
) -> Result<NormalizedProv> {
    let mut doc = ProvDocument::new();
    let mut derived_relations = Vec::new();
    let mut agent_labels = HashMap::new();

    match event.data() {
        ProvEventData::LlmCallStarted {
            scope,
            client,
            model,
            function_name,
            prompt,
            metadata,
        } => {
            let scope_key = build_call_scope_key(event, scope, metadata)?;
            let ordinal =
                get_ordinal_for_call(call_state, &scope_key, true, CallOrdinalChannel::Llm);
            let scope_key_str = scope_key.to_string();
            let activity_id = llm_activity_id(&scope_key_str, ordinal);
            let mut attrs = base_attrs(event);
            let (resolved_client, resolved_model) =
                canonicalize_llm_client_model(event, client, model, prompt, metadata)?;
            attrs.insert(a2a::CLIENT.to_string(), Value::String(resolved_client));
            attrs.insert(a2a::MODEL.to_string(), Value::String(resolved_model));
            {
                let fid = BamlFunctionId::parse(function_name);
                attrs.insert(
                    a2a::FUNCTION_NAME.to_string(),
                    Value::String(fid.full_name()),
                );
                attrs.insert(
                    a2a::PROMPT_NAME.to_string(),
                    Value::String(fid.prompt_name().as_str().to_string()),
                );
            }
            attrs.insert(
                a2a::AGENT_ID.to_string(),
                Value::String(scope_key.agent.clone()),
            );
            if let Some(plan_id) = metadata_string_field(metadata, "plan_id") {
                attrs.insert(a2a::PLAN_ID.to_string(), Value::String(plan_id));
            }
            if let Some(step_id) = metadata_string_field(metadata, "step_id") {
                attrs.insert(a2a::STEP_ID.to_string(), Value::String(step_id));
            }
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attrs.insert(
                    a2a::MESSAGE_ID.to_string(),
                    Value::String(message_id.as_str().to_string()),
                );
            }
            attrs.insert(
                a2a::LLM_CALL_PAYLOAD_ID.to_string(),
                Value::String(payload_id_for_event(event, "llm_call")),
            );
            let start_time_ms = Some(event.timestamp_ms());

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms,
                    end_time_ms: None,
                    prov_type: Some(prov_type::<LlmCallActivityId>()),
                    attributes: attrs,
                },
            );

            let prompt_id = llm_prompt_entity_id(&scope_key_str, ordinal);
            let mut prompt_attrs = base_attrs(event);
            prompt_attrs.insert(a2a::PROMPT.to_string(), prompt.clone());
            doc.insert_entity(
                prompt_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<LlmPromptEntityId>()),
                    attributes: prompt_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                prompt_id,
                Some(a2a_roles::PROMPT.to_string()),
            );
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                &mut agent_labels,
                context,
            )?;
        }
        ProvEventData::LlmCallCompleted {
            scope,
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
        } => {
            let scope_key = build_call_scope_key(event, scope, metadata)?;
            let ordinal =
                get_ordinal_for_call(call_state, &scope_key, false, CallOrdinalChannel::Llm);
            let scope_key_str = scope_key.to_string();
            call_state.llm_event_to_scope_ordinal.insert(
                event.id().as_str().to_string(),
                (scope_key.clone(), ordinal),
            );
            let activity_id = llm_activity_id(&scope_key_str, ordinal);
            let mut attrs = base_attrs(event);
            let (resolved_client, resolved_model) =
                canonicalize_llm_client_model(event, client, model, prompt, metadata)?;
            attrs.insert(a2a::CLIENT.to_string(), Value::String(resolved_client));
            attrs.insert(a2a::MODEL.to_string(), Value::String(resolved_model));
            {
                let fid = BamlFunctionId::parse(function_name);
                attrs.insert(
                    a2a::FUNCTION_NAME.to_string(),
                    Value::String(fid.full_name()),
                );
                attrs.insert(
                    a2a::PROMPT_NAME.to_string(),
                    Value::String(fid.prompt_name().as_str().to_string()),
                );
            }
            attrs.insert(
                a2a::AGENT_ID.to_string(),
                Value::String(scope_key.agent.clone()),
            );
            if let Some(plan_id) = metadata_string_field(metadata, "plan_id") {
                attrs.insert(a2a::PLAN_ID.to_string(), Value::String(plan_id));
            }
            if let Some(step_id) = metadata_string_field(metadata, "step_id") {
                attrs.insert(a2a::STEP_ID.to_string(), Value::String(step_id));
            }
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attrs.insert(
                    a2a::MESSAGE_ID.to_string(),
                    Value::String(message_id.as_str().to_string()),
                );
            }
            attrs.insert(
                a2a::LLM_CALL_PAYLOAD_ID.to_string(),
                Value::String(payload_id_for_event(event, "llm_call")),
            );
            attrs.insert(
                a2a::LLM_RESULT_PAYLOAD_ID.to_string(),
                Value::String(payload_id_for_event(event, "llm_result")),
            );
            if let Some(result) = metadata_json_field(metadata, "result") {
                attrs.insert(a2a::RESULT.to_string(), result);
            }
            if let Some(error) = metadata_json_field(metadata, "error") {
                attrs.insert(a2a::ERROR.to_string(), error);
            }
            match usage {
                crate::events::LlmUsage::Known {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    cached_input_tokens,
                } => {
                    attrs.insert(
                        a2a::USAGE_PROMPT_TOKENS.to_string(),
                        Value::Number((*prompt_tokens).into()),
                    );
                    attrs.insert(
                        a2a::USAGE_COMPLETION_TOKENS.to_string(),
                        Value::Number((*completion_tokens).into()),
                    );
                    attrs.insert(
                        a2a::USAGE_TOTAL_TOKENS.to_string(),
                        Value::Number((*total_tokens).into()),
                    );
                    if let Some(cached) = cached_input_tokens {
                        attrs.insert(
                            a2a::USAGE_CACHED_INPUT_TOKENS.to_string(),
                            Value::Number((*cached).into()),
                        );
                    }
                }
                crate::events::LlmUsage::Unknown => {}
            }
            attrs.insert(
                a2a::DURATION_MS.to_string(),
                Value::Number((*duration_ms).into()),
            );
            if let Some(drift) = drift {
                attrs.insert(a2a::DRIFT_SCORE.to_string(), serde_json::json!(drift.score));
                attrs.insert(
                    a2a::DRIFT_SEVERITY.to_string(),
                    Value::String(drift.severity.as_str().to_owned()),
                );
                attrs.insert(
                    a2a::DRIFT_MODE.to_string(),
                    Value::String(
                        serde_json::to_string(&drift.mode)
                            .unwrap_or_default()
                            .trim_matches('"')
                            .to_owned(),
                    ),
                );
                attrs.insert(
                    a2a::DRIFT_WARN_MIN_SCORE.to_string(),
                    serde_json::json!(drift.warn_min_score),
                );
                attrs.insert(
                    a2a::DRIFT_BLOCK_MIN_SCORE.to_string(),
                    serde_json::json!(drift.block_min_score),
                );
                attrs.insert(
                    a2a::INTENT_TEXT_PREVIEW.to_string(),
                    Value::String(drift.intent_text_preview.clone()),
                );
                attrs.insert(
                    a2a::RESPONSE_TEXT_PREVIEW.to_string(),
                    Value::String(drift.response_text_preview.clone()),
                );
                if !drift.step_text_preview.is_empty() {
                    attrs.insert(
                        a2a::STEP_TEXT_PREVIEW.to_string(),
                        Value::String(drift.step_text_preview.clone()),
                    );
                }
                if let Some(ref plan_drift) = drift.plan_drift {
                    use crate::events::LlmPlanDriftInfo;
                    attrs.insert(
                        a2a::PLAN_DRIFT_INTENT_ALIGNMENT.to_string(),
                        serde_json::json!(plan_drift.intent_alignment()),
                    );
                    attrs.insert(
                        a2a::PLAN_DRIFT_TRAJECTORY.to_string(),
                        serde_json::json!(plan_drift.trajectory_drift()),
                    );
                    attrs.insert(
                        a2a::PLAN_DRIFT_ADHERENCE.to_string(),
                        serde_json::json!(plan_drift.plan_adherence_score()),
                    );
                    attrs.insert(
                        a2a::PLAN_DRIFT_COMPOSITE_SEVERITY.to_string(),
                        Value::String(plan_drift.composite_severity().as_str().to_owned()),
                    );
                    // PlanCommitted-only fields: step alignment and XE score.
                    if let LlmPlanDriftInfo::PlanCommitted {
                        step_alignment,
                        cross_encoder_step_score,
                        ..
                    } = plan_drift
                    {
                        attrs.insert(
                            a2a::PLAN_DRIFT_STEP_ALIGNMENT.to_string(),
                            serde_json::json!(step_alignment),
                        );
                        attrs.insert(
                            a2a::PLAN_DRIFT_CROSS_ENCODER_STEP.to_string(),
                            serde_json::json!(cross_encoder_step_score),
                        );
                    }
                }
                if let Some(ref cd) = drift.citation_drift
                    && let Ok(v) = serde_json::to_value(cd)
                {
                    attrs.insert(a2a::CITATION_DRIFT.to_string(), v);
                }
            }
            if !citations.is_empty() {
                attrs.insert(
                    "citations".to_string(),
                    serde_json::Value::Array(
                        citations
                            .iter()
                            .map(|c| serde_json::Value::String(c.as_str().to_string()))
                            .collect(),
                    ),
                );
            }
            for rc in resolved_citations {
                // Write-time cross-reference: the target node was created in a prior
                // normalization pass using SessionStepEntityId conventions. We reconstruct
                // the same ID using the target_node_id which was built by the effect
                // subscriber at event emission time.
                let target_entity = ProvEntityId::from_write_time_node_id(&rc.target_node_id);
                let mut edge_attrs = HashMap::new();
                edge_attrs.insert(
                    "prov_type".to_string(),
                    Value::String(a2a_relation_types::CITED.to_string()),
                );
                if !rc.raw.is_empty() {
                    edge_attrs.insert("raw".to_string(), Value::String(rc.raw.clone()));
                }
                if rc.negated {
                    edge_attrs.insert("negated".to_string(), Value::Bool(true));
                }
                if let Some(sim) = rc.similarity {
                    edge_attrs.insert("similarity".to_string(), serde_json::json!(sim));
                }
                if let Some(ls) = rc.line_start {
                    edge_attrs.insert("line_start".to_string(), Value::from(ls as u64));
                }
                if let Some(le) = rc.line_end {
                    edge_attrs.insert("line_end".to_string(), Value::from(le as u64));
                }
                derived_relations.push(A2aDerivedRelation {
                    relation: A2aRelationType::CitedSource,
                    from: ProvNodeRef::Activity(activity_id.clone()),
                    to: ProvNodeRef::Entity(target_entity),
                    attributes: edge_attrs,
                });
            }
            let is_success = bool::from(*outcome);
            let failure_resolution = if is_success {
                None
            } else {
                Some(classify_failure_from_metadata(metadata))
            };
            attrs.insert(
                a2a::ACTIVITY_OUTCOME.to_string(),
                Value::String(if is_success {
                    "Success".to_string()
                } else {
                    "Failed".to_string()
                }),
            );
            if let Some(resolution) = &failure_resolution {
                attrs.insert(
                    a2a::FAILURE_CLASS.to_string(),
                    Value::String(resolution.class.clone()),
                );
                attrs.insert(
                    a2a::FAILURE_EVIDENCE.to_string(),
                    Value::String(resolution.evidence.clone()),
                );
            }

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms: None,
                    end_time_ms: Some(event.timestamp_ms()),
                    prov_type: Some(prov_type::<LlmCallActivityId>()),
                    attributes: attrs,
                },
            );

            let prompt_id = llm_prompt_entity_id(&scope_key_str, ordinal);
            let mut prompt_attrs = base_attrs(event);
            prompt_attrs.insert(a2a::PROMPT.to_string(), prompt.clone());
            doc.insert_entity(
                prompt_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<LlmPromptEntityId>()),
                    attributes: prompt_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                prompt_id,
                Some(a2a_roles::PROMPT.to_string()),
            );
            if !is_success {
                let resolution = failure_resolution
                    .as_ref()
                    .expect("failed outcome must carry failure resolution");
                let evidence_prompt_id = llm_prompt_entity_id(&scope_key_str, ordinal);
                upsert_failure_classification(
                    &mut doc,
                    event,
                    FailureClassificationTarget {
                        call_kind: "llm_call",
                        scope_key: scope_key.clone(),
                        ordinal,
                        failed_activity_id: activity_id.clone(),
                        evidence_entity: Some(evidence_prompt_id),
                    },
                    resolution,
                );
            }
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                &mut agent_labels,
                context,
            )?;
        }
        ProvEventData::PromptRejected {
            scope,
            llm_call_activity_anchor,
            reason,
        } => {
            let (scope_key, ordinal) = call_state
                .llm_event_to_scope_ordinal
                .get(llm_call_activity_anchor.as_str())
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason:
                        "PromptRejected requires prior LlmCallCompleted in same normalizer session"
                            .to_string(),
                })?
                .clone();
            let scope_key_str = scope_key.to_string();
            let activity_id = prompt_rejected_activity_id(&scope_key_str, ordinal);
            let mut attrs = base_attrs(event);
            attrs.insert(a2a::REASON.to_string(), Value::String(reason.clone()));
            let ts = event.timestamp_ms();
            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms: Some(ts),
                    end_time_ms: Some(ts),
                    prov_type: Some(prov_type::<PromptRejectedActivityId>()),
                    attributes: attrs,
                },
            );
            let prompt_id = llm_prompt_entity_id(&scope_key_str, ordinal);
            insert_used(
                &mut doc,
                activity_id.clone(),
                prompt_id,
                Some(a2a_roles::REJECTED_OUTPUT.to_string()),
            );
            let reason_lower = reason.to_ascii_lowercase();
            let evidence = {
                let trimmed = reason.trim();
                if trimmed.is_empty() {
                    "linked_prompt_rejected".to_string()
                } else {
                    trimmed.to_string()
                }
            };
            let resolution = if reason_lower.contains("baml")
                || reason_lower.contains("schema")
                || reason_lower.contains("validation")
            {
                FailureResolution {
                    class: "baml_validation_error".to_string(),
                    evidence: evidence.clone(),
                    code: Some("baml_validation".to_string()),
                }
            } else if reason_lower.contains("policy") || reason_lower.contains("reject") {
                FailureResolution {
                    class: "prompt_rejected".to_string(),
                    evidence: evidence.clone(),
                    code: Some("prompt_rejected".to_string()),
                }
            } else {
                FailureResolution {
                    class: "prompt_rejected".to_string(),
                    evidence,
                    code: None,
                }
            };
            let failed_llm_activity = llm_activity_id(&scope_key_str, ordinal);
            let evidence_prompt_id = llm_prompt_entity_id(&scope_key_str, ordinal);
            upsert_failure_classification(
                &mut doc,
                event,
                FailureClassificationTarget {
                    call_kind: "llm_call",
                    scope_key: scope_key.clone(),
                    ordinal,
                    failed_activity_id: failed_llm_activity,
                    evidence_entity: Some(evidence_prompt_id),
                },
                &resolution,
            );
            if let CallScope::Message { message_id } = scope {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                &mut agent_labels,
                context,
            )?;
        }
        ProvEventData::ToolCallStarted {
            scope,
            tool_name,
            function_name,
            args,
            metadata,
            delegation_target,
        } => {
            let scope_key = build_call_scope_key(event, scope, metadata)?;
            let ordinal =
                get_ordinal_for_call(call_state, &scope_key, true, CallOrdinalChannel::Tool);
            let scope_key_str = scope_key.to_string();
            let activity_id = tool_activity_id(&scope_key_str, ordinal);
            let mut attrs = base_attrs(event);
            attrs.insert(a2a::TOOL_NAME.to_string(), Value::String(tool_name.clone()));
            if let Some(function_name) = function_name {
                attrs.insert(
                    a2a::FUNCTION_NAME.to_string(),
                    Value::String(function_name.clone()),
                );
            }
            attrs.insert(
                a2a::AGENT_ID.to_string(),
                Value::String(scope_key.agent.clone()),
            );
            if let Some(plan_id) = metadata_string_field(metadata, "plan_id") {
                attrs.insert(a2a::PLAN_ID.to_string(), Value::String(plan_id));
            }
            if let Some(step_id) = metadata_string_field(metadata, "step_id") {
                attrs.insert(a2a::STEP_ID.to_string(), Value::String(step_id));
            }
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attrs.insert(
                    a2a::MESSAGE_ID.to_string(),
                    Value::String(message_id.as_str().to_string()),
                );
            }
            attrs.insert(
                a2a::TOOL_CALL_PAYLOAD_ID.to_string(),
                Value::String(payload_id_for_event(event, "tool_call")),
            );
            if let Some(phase) = metadata_string_field(metadata, "phase") {
                attrs.insert(a2a::PHASE.to_string(), Value::String(phase));
            }
            let start_time_ms = Some(event.timestamp_ms());

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms,
                    end_time_ms: None,
                    prov_type: Some(prov_type::<ToolCallActivityId>()),
                    attributes: attrs,
                },
            );

            let args_id = tool_args_entity_id(&scope_key_str, ordinal);
            let mut args_attrs = base_attrs(event);
            args_attrs.insert(a2a::ARGS.to_string(), args.clone());
            doc.insert_entity(
                args_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<ToolArgsEntityId>()),
                    attributes: args_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                args_id,
                Some(a2a_roles::ARGS.to_string()),
            );
            if let Some(agent_package) = delegation_target {
                let delegation_id = delegation_target_entity_id(&scope_key_str, ordinal);
                let mut delegation_attrs = base_attrs(event);
                delegation_attrs.insert(
                    a2a::DELEGATION_TARGET.to_string(),
                    Value::String(agent_package.clone()),
                );
                doc.insert_entity(
                    delegation_id.clone(),
                    Entity {
                        prov_type: Some(prov_type::<DelegationTargetEntityId>()),
                        attributes: delegation_attrs,
                    },
                );
                insert_used(
                    &mut doc,
                    activity_id.clone(),
                    delegation_id,
                    Some(a2a_roles::DELEGATION_TARGET.to_string()),
                );
            }
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                &mut agent_labels,
                context,
            )?;
        }
        ProvEventData::ToolCallCompleted {
            scope,
            tool_name,
            function_name,
            args,
            metadata,
            duration_ms,
            outcome,
            delegation_target,
        } => {
            let scope_key = build_call_scope_key(event, scope, metadata)?;
            let ordinal =
                get_ordinal_for_call(call_state, &scope_key, false, CallOrdinalChannel::Tool);
            let scope_key_str = scope_key.to_string();
            let activity_id = tool_activity_id(&scope_key_str, ordinal);
            let mut attrs = base_attrs(event);
            attrs.insert(a2a::TOOL_NAME.to_string(), Value::String(tool_name.clone()));
            if let Some(function_name) = function_name {
                attrs.insert(
                    a2a::FUNCTION_NAME.to_string(),
                    Value::String(function_name.clone()),
                );
            }
            attrs.insert(
                a2a::AGENT_ID.to_string(),
                Value::String(scope_key.agent.clone()),
            );
            if let Some(plan_id) = metadata_string_field(metadata, "plan_id") {
                attrs.insert(a2a::PLAN_ID.to_string(), Value::String(plan_id));
            }
            if let Some(step_id) = metadata_string_field(metadata, "step_id") {
                attrs.insert(a2a::STEP_ID.to_string(), Value::String(step_id));
            }
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attrs.insert(
                    a2a::MESSAGE_ID.to_string(),
                    Value::String(message_id.as_str().to_string()),
                );
            }
            attrs.insert(
                a2a::TOOL_CALL_PAYLOAD_ID.to_string(),
                Value::String(payload_id_for_event(event, "tool_call")),
            );
            attrs.insert(
                a2a::TOOL_RESULT_PAYLOAD_ID.to_string(),
                Value::String(payload_id_for_event(event, "tool_result")),
            );
            if let Some(phase) = metadata_string_field(metadata, "phase") {
                attrs.insert(a2a::PHASE.to_string(), Value::String(phase));
            }
            if let Some(result) = metadata_json_field(metadata, "result") {
                attrs.insert(a2a::RESULT.to_string(), result);
            }
            if let Some(error) = metadata_json_field(metadata, "error") {
                attrs.insert(a2a::ERROR.to_string(), error);
            }
            attrs.insert(
                a2a::DURATION_MS.to_string(),
                Value::Number((*duration_ms).into()),
            );
            let is_success = bool::from(*outcome);
            let failure_resolution = if is_success {
                None
            } else {
                Some(classify_failure_from_metadata(metadata))
            };
            attrs.insert(
                a2a::ACTIVITY_OUTCOME.to_string(),
                Value::String(if is_success {
                    "Success".to_string()
                } else {
                    "Failed".to_string()
                }),
            );
            if let Some(resolution) = &failure_resolution {
                attrs.insert(
                    a2a::FAILURE_CLASS.to_string(),
                    Value::String(resolution.class.clone()),
                );
                attrs.insert(
                    a2a::FAILURE_EVIDENCE.to_string(),
                    Value::String(resolution.evidence.clone()),
                );
            }

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms: None,
                    end_time_ms: Some(event.timestamp_ms()),
                    prov_type: Some(prov_type::<ToolCallActivityId>()),
                    attributes: attrs,
                },
            );
            call_state
                .tool_call_anchor_to_activity
                .insert(event.id().as_str().to_string(), activity_id.clone());

            let args_id = tool_args_entity_id(&scope_key_str, ordinal);
            let mut args_attrs = base_attrs(event);
            args_attrs.insert(a2a::ARGS.to_string(), args.clone());
            doc.insert_entity(
                args_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<ToolArgsEntityId>()),
                    attributes: args_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                args_id,
                Some(a2a_roles::ARGS.to_string()),
            );
            if !is_success {
                let resolution = failure_resolution
                    .as_ref()
                    .expect("failed outcome must carry failure resolution");
                let evidence_args_id = tool_args_entity_id(&scope_key_str, ordinal);
                upsert_failure_classification(
                    &mut doc,
                    event,
                    FailureClassificationTarget {
                        call_kind: "tool_call",
                        scope_key: scope_key.clone(),
                        ordinal,
                        failed_activity_id: activity_id.clone(),
                        evidence_entity: Some(evidence_args_id),
                    },
                    resolution,
                );
            }
            if let Some(agent_package) = delegation_target {
                let delegation_id = delegation_target_entity_id(&scope_key_str, ordinal);
                let mut delegation_attrs = base_attrs(event);
                delegation_attrs.insert(
                    a2a::DELEGATION_TARGET.to_string(),
                    Value::String(agent_package.clone()),
                );
                doc.insert_entity(
                    delegation_id.clone(),
                    Entity {
                        prov_type: Some(prov_type::<DelegationTargetEntityId>()),
                        attributes: delegation_attrs,
                    },
                );
                insert_used(
                    &mut doc,
                    activity_id.clone(),
                    delegation_id,
                    Some(a2a_roles::DELEGATION_TARGET.to_string()),
                );
            }
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                &mut agent_labels,
                context,
            )?;
        }
        ProvEventData::AgentBooted {
            agent_id,
            agent_type,
            agent_version,
            archive_path,
        } => {
            agent_registry.insert(agent_id.as_str().to_string());
            // Create AgentArchive entity
            let archive_entity_id = archive_entity_id(archive_path);
            let mut archive_attrs = base_attrs(event);
            archive_attrs.insert(
                a2a::ARCHIVE_PATH.to_string(),
                Value::String(archive_path.clone()),
            );
            doc.insert_entity(
                archive_entity_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<ArchiveEntityId>()),
                    attributes: archive_attrs,
                },
            );

            // Create AgentBoot activity (agent_id derivable from id: boot:{agent_id})
            let boot_activity_id = boot_activity_id(agent_id);
            let mut boot_attrs = base_attrs(event);
            boot_attrs.insert(
                a2a::AGENT_TYPE.to_string(),
                Value::String(agent_type.as_str().to_string()),
            );
            boot_attrs.insert(
                a2a::AGENT_VERSION.to_string(),
                Value::String(agent_version.clone()),
            );
            doc.insert_activity(
                boot_activity_id.clone(),
                Activity {
                    start_time_ms: Some(event.timestamp_ms()),
                    end_time_ms: Some(event.timestamp_ms()),
                    prov_type: Some(prov_type::<AgentBootActivityId>()),
                    attributes: boot_attrs,
                },
            );

            // Link archive --USED--> boot
            insert_used(
                &mut doc,
                boot_activity_id.clone(),
                archive_entity_id,
                Some(a2a_roles::ARCHIVE.to_string()),
            );

            let instance_agent_id = agent_runtime_instance_id(agent_id);
            let mut instance_attrs = base_attrs(event);
            instance_attrs.insert(
                a2a::AGENT_ID.to_string(),
                Value::String(agent_id.as_str().to_string()),
            );
            instance_attrs.insert(
                a2a::AGENT_TYPE.to_string(),
                Value::String(agent_type.as_str().to_string()),
            );
            instance_attrs.insert(
                a2a::AGENT_VERSION.to_string(),
                Value::String(agent_version.clone()),
            );
            instance_attrs.insert(
                a2a::ARCHIVE_PATH.to_string(),
                Value::String(archive_path.clone()),
            );
            doc.insert_agent(
                instance_agent_id.clone(),
                Agent {
                    prov_type: Some(prov_type::<AgentRuntimeInstanceId>()),
                    attributes: instance_attrs,
                },
            );

            insert_was_generated_by(
                &mut doc,
                ProvNodeRef::Agent(instance_agent_id.clone()),
                boot_activity_id.clone(),
                Some(event.timestamp_ms()),
            );
            insert_qualified_generation(
                &mut doc,
                ProvNodeRef::Agent(instance_agent_id.clone()),
                boot_activity_id.clone(),
                Some(event.timestamp_ms()),
            );

            // Link boot activity to runner runtime instance via association role.
            let runner_runtime_id = runner_runtime_instance_id();
            ensure_runner_runtime_instance(&mut doc);
            insert_was_associated_with(
                &mut doc,
                boot_activity_id,
                runner_runtime_id,
                Some(prov_roles::EXECUTING_AGENT.to_string()),
            );
        }
        ProvEventData::TaskExists {
            task_id,
            context_id: _,
        } => {
            let _ = ensure_task_entity(&mut doc, task_id);
        }
        ProvEventData::TaskExecutionStarted {
            task_id,
            agent_id,
            context_id,
        } => {
            let task_entity = ensure_task_entity(&mut doc, task_id);

            let task_execution = ensure_task_execution_activity(
                &mut doc,
                task_id,
                context_id,
                Some(event.timestamp_ms()),
                None,
                &mut agent_labels,
                Some(agent_id),
                context,
            )?;
            insert_was_generated_by(
                &mut doc,
                ProvNodeRef::Entity(task_entity.clone()),
                task_execution.clone(),
                Some(event.timestamp_ms()),
            );

            let agent_instance_id = get_agent_runtime_instance(&doc, agent_id, &mut agent_labels)?;
            insert_was_associated_with(
                &mut doc,
                task_execution.clone(),
                agent_instance_id,
                Some(prov_roles::EXECUTING_AGENT.to_string()),
            );

            let invoking_agent_id = runner_runtime_instance_id();
            ensure_runner_runtime_instance(&mut doc);
            insert_was_associated_with(
                &mut doc,
                task_execution,
                invoking_agent_id,
                Some(prov_roles::INVOKING_AGENT.to_string()),
            );
        }
        ProvEventData::TaskExecutionEnded {
            task_id,
            context_id,
        } => {
            let _ = ensure_task_entity(&mut doc, task_id);
            let _ = ensure_task_execution_activity(
                &mut doc,
                task_id,
                context_id,
                None,
                Some(event.timestamp_ms()),
                &mut agent_labels,
                None,
                context,
            )?;
        }
        ProvEventData::TaskStatusChanged {
            task_id,
            old_status,
            new_status,
        } => {
            let Some(new_status) = new_status.as_deref() else {
                return Ok(NormalizedProv {
                    document: doc,
                    derived_relations,
                    agent_labels,
                });
            };
            let _task_entity = ensure_task_entity(&mut doc, task_id);
            let is_terminal = is_terminal_status(new_status);
            let task_execution = ensure_task_execution_activity(
                &mut doc,
                task_id,
                event.context_id(),
                None,
                is_terminal.then_some(event.timestamp_ms()),
                &mut agent_labels,
                None,
                context,
            )?;
            let status_id = task_state_entity_id(task_id, new_status);
            let mut status_attrs = base_attrs(event);
            status_attrs.insert(
                a2a::TASK_STATE_TIME.to_string(),
                Value::Number(event.timestamp_ms().into()),
            );
            status_attrs.insert(
                a2a::TASK_STATE.to_string(),
                Value::String(new_status.to_string()),
            );
            if let Some(old_status) = old_status.as_deref() {
                status_attrs.insert(
                    a2a::OLD_STATUS.to_string(),
                    Value::String(old_status.to_string()),
                );
            }
            doc.insert_entity(
                status_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<TaskStateEntityId>()),
                    attributes: status_attrs,
                },
            );
            insert_used(
                &mut doc,
                task_execution.clone(),
                status_id.clone(),
                Some(a2a_roles::TASK_STATE.to_string()),
            );

            if is_terminal {
                let task_entity = task_entity_id(task_id);
                insert_was_generated_by(
                    &mut doc,
                    ProvNodeRef::Entity(task_entity),
                    task_execution.clone(),
                    Some(event.timestamp_ms()),
                );
            }

            if let Some(old_status) = old_status.as_deref() {
                let old_id = task_state_entity_id(task_id, old_status);
                let mut old_attrs = HashMap::new();
                old_attrs.insert(
                    a2a::CONTEXT_ID.to_string(),
                    Value::String(event.context_id().as_str().to_string()),
                );
                old_attrs.insert(
                    a2a::TASK_ID.to_string(),
                    Value::String(task_id.as_str().to_string()),
                );
                old_attrs.insert(
                    a2a::TASK_STATE_TIME.to_string(),
                    Value::Number(event.timestamp_ms().saturating_sub(1).into()),
                );
                old_attrs.insert(
                    a2a::TASK_STATE.to_string(),
                    Value::String(old_status.to_string()),
                );
                doc.insert_entity(
                    old_id.clone(),
                    Entity {
                        prov_type: Some(prov_type::<TaskStateEntityId>()),
                        attributes: old_attrs,
                    },
                );
                insert_was_derived_from(
                    &mut doc,
                    status_id.clone(),
                    old_id.clone(),
                    Some(task_execution.clone()),
                    Some(a2a_relation_types::STATUS_TRANSITION.to_string()),
                );
                derived_relations.push(A2aDerivedRelation {
                    relation: A2aRelationType::TaskStatusTransition,
                    from: ProvNodeRef::Entity(old_id),
                    to: ProvNodeRef::Entity(status_id),
                    attributes: derived_attrs(event),
                });
            }
        }
        ProvEventData::TaskArtifactGenerated {
            task_id,
            artifact_id,
            artifact_type,
        } => {
            let task_entity = ensure_task_entity(&mut doc, task_id);
            let task_execution = ensure_task_execution_activity(
                &mut doc,
                task_id,
                event.context_id(),
                None,
                None,
                &mut agent_labels,
                None,
                context,
            )?;
            let artifact_id_str =
                artifact_entity_id(task_id, artifact_id, artifact_type, event.id());
            let mut artifact_attrs = base_attrs(event);
            if let Some(artifact_id) = artifact_id {
                artifact_attrs.insert(
                    a2a::ARTIFACT_ID.to_string(),
                    Value::String(artifact_id.as_str().to_string()),
                );
            }
            if let Some(artifact_type) = artifact_type {
                artifact_attrs.insert(
                    a2a::ARTIFACT_TYPE.to_string(),
                    Value::String(artifact_type.clone()),
                );
            }
            doc.insert_entity(
                artifact_id_str.clone(),
                Entity {
                    prov_type: Some(if artifact_id.is_some() {
                        prov_type::<ArtifactByIdEntityId>()
                    } else if artifact_type.is_some() {
                        prov_type::<ArtifactByTypeEntityId>()
                    } else {
                        prov_type::<ArtifactByActivityAnchorEntityId>()
                    }),
                    attributes: artifact_attrs,
                },
            );
            insert_was_generated_by(
                &mut doc,
                ProvNodeRef::Entity(artifact_id_str.clone()),
                task_execution.clone(),
                Some(event.timestamp_ms()),
            );
            derived_relations.push(A2aDerivedRelation {
                relation: A2aRelationType::TaskHasArtifact,
                from: ProvNodeRef::Entity(task_entity),
                to: ProvNodeRef::Entity(artifact_id_str),
                attributes: derived_attrs(event),
            });
        }
        ProvEventData::IntentResolved {
            task_id,
            intent_id,
            description,
            citations,
            supersession,
            revision_intent_drift,
        } => {
            let task_entity = ensure_task_entity(&mut doc, task_id);
            let intent_entity = intent_entity_id(task_id, intent_id);
            let mut intent_attrs = base_attrs(event);
            intent_attrs.insert(
                a2a::INTENT_ID.to_string(),
                Value::String(intent_id.to_string()),
            );
            intent_attrs.insert(
                a2a::STATUS.to_string(),
                Value::String("resolved".to_string()),
            );
            intent_attrs.insert(prov::LABEL.to_string(), Value::String(description.clone()));
            if let Some(drift_score) = revision_intent_drift {
                intent_attrs.insert(
                    a2a::REVISION_INTENT_DRIFT.to_string(),
                    serde_json::json!(drift_score),
                );
            }
            // Store raw citation strings on the entity for downstream resolution (Phase 2).
            if !citations.is_empty() {
                intent_attrs.insert(
                    "citations".to_string(),
                    serde_json::Value::Array(
                        citations
                            .iter()
                            .map(|c| serde_json::Value::String(c.as_str().to_string()))
                            .collect(),
                    ),
                );
            }
            doc.insert_entity(
                intent_entity.clone(),
                Entity {
                    prov_type: Some(prov_type::<IntentEntityId>()),
                    attributes: intent_attrs,
                },
            );

            derived_relations.push(A2aDerivedRelation {
                relation: A2aRelationType::HasIntent,
                from: ProvNodeRef::Entity(task_entity),
                to: ProvNodeRef::Entity(intent_entity.clone()),
                attributes: HashMap::new(),
            });

            let task_key = task_id.as_str().to_string();
            if let Some(previous) = call_state
                .latest_intent_entity_by_task
                .get(&task_key)
                .cloned()
                && previous != intent_entity
            {
                derived_relations.push(A2aDerivedRelation {
                    relation: match supersession.unwrap_or(PlanningSupersessionKind::ReplacedBy) {
                        PlanningSupersessionKind::ReplacedBy => A2aRelationType::IntentReplacedBy,
                        PlanningSupersessionKind::RefinedBy => A2aRelationType::IntentRefinedBy,
                    },
                    from: ProvNodeRef::Entity(previous),
                    to: ProvNodeRef::Entity(intent_entity.clone()),
                    attributes: base_attrs(event),
                });
            }
            call_state
                .latest_intent_entity_by_task
                .insert(task_key, intent_entity);
        }
        ProvEventData::PlanGenerated {
            task_id,
            intent_id,
            plan_id,
            steps,
            supersession,
        } => {
            let task_entity = ensure_task_entity(&mut doc, task_id);
            let intent_entity = intent_entity_id(task_id, intent_id);
            let mut intent_attrs = doc
                .entity(&intent_entity)
                .map(|entity| entity.attributes.clone())
                .unwrap_or_default();
            intent_attrs.insert(
                a2a::INTENT_ID.to_string(),
                Value::String(intent_id.to_string()),
            );
            doc.insert_entity(
                intent_entity.clone(),
                Entity {
                    prov_type: Some(prov_type::<IntentEntityId>()),
                    attributes: intent_attrs,
                },
            );

            let plan_entity = plan_entity_id(task_id, plan_id);
            let mut plan_attrs = base_attrs(event);
            plan_attrs.insert(
                a2a::INTENT_ID.to_string(),
                Value::String(intent_id.to_string()),
            );
            plan_attrs.insert(a2a::PLAN_ID.to_string(), Value::String(plan_id.to_string()));
            plan_attrs.insert(
                a2a::STATUS.to_string(),
                Value::String("generated".to_string()),
            );
            doc.insert_entity(
                plan_entity.clone(),
                Entity {
                    prov_type: Some(prov_type::<PlanEntityId>()),
                    attributes: plan_attrs,
                },
            );
            insert_was_derived_from(&mut doc, plan_entity.clone(), intent_entity, None, None);

            derived_relations.push(A2aDerivedRelation {
                relation: A2aRelationType::HasPlan,
                from: ProvNodeRef::Entity(task_entity),
                to: ProvNodeRef::Entity(plan_entity.clone()),
                attributes: HashMap::new(),
            });

            for step in steps {
                let step_entity = plan_step_entity_id(task_id, plan_id, &step.step_id);
                let mut step_attrs = base_attrs(event);
                step_attrs.insert(a2a::PLAN_ID.to_string(), Value::String(plan_id.to_string()));
                step_attrs.insert(
                    a2a::STEP_ID.to_string(),
                    Value::String(step.step_id.to_string()),
                );
                step_attrs.insert(
                    prov::LABEL.to_string(),
                    Value::String(step.description.clone()),
                );
                step_attrs.insert(a2a::STATUS.to_string(), Value::String("ready".to_string()));
                step_attrs.insert(
                    a2a::DEPENDS_ON.to_string(),
                    Value::Array(
                        step.depends_on
                            .iter()
                            .map(|dep| Value::String(dep.to_string()))
                            .collect(),
                    ),
                );
                step_attrs.insert(
                    a2a::STEP_ORDER.to_string(),
                    Value::Number((step.order as u64).into()),
                );
                doc.insert_entity(
                    step_entity.clone(),
                    Entity {
                        prov_type: Some(prov_type::<PlanStepEntityId>()),
                        attributes: step_attrs,
                    },
                );
                insert_was_derived_from(&mut doc, step_entity, plan_entity.clone(), None, None);
            }

            let task_key = task_id.as_str().to_string();
            if let Some(previous) = call_state
                .latest_plan_entity_by_task
                .get(&task_key)
                .cloned()
                && previous != plan_entity
            {
                derived_relations.push(A2aDerivedRelation {
                    relation: match supersession.unwrap_or(PlanningSupersessionKind::ReplacedBy) {
                        PlanningSupersessionKind::ReplacedBy => A2aRelationType::PlanReplacedBy,
                        PlanningSupersessionKind::RefinedBy => A2aRelationType::PlanRefinedBy,
                    },
                    from: ProvNodeRef::Entity(previous),
                    to: ProvNodeRef::Entity(plan_entity.clone()),
                    attributes: base_attrs(event),
                });
            }
            call_state
                .latest_plan_entity_by_task
                .insert(task_key, plan_entity);
        }
        ProvEventData::PlanStepStatusChanged {
            task_id,
            intent_id,
            plan_id,
            step_id,
            old_status,
            new_status,
            citations,
        } => {
            let _task_entity = ensure_task_entity(&mut doc, task_id);
            let step_entity = plan_step_entity_id(task_id, plan_id, step_id);
            let mut step_attrs = doc
                .entity(&step_entity)
                .map(|entity| entity.attributes.clone())
                .unwrap_or_default();
            step_attrs.insert(
                a2a::INTENT_ID.to_string(),
                Value::String(intent_id.to_string()),
            );
            step_attrs.insert(a2a::PLAN_ID.to_string(), Value::String(plan_id.to_string()));
            step_attrs.insert(a2a::STEP_ID.to_string(), Value::String(step_id.to_string()));
            step_attrs.insert(a2a::STATUS.to_string(), Value::String(new_status.clone()));
            // Store raw citation strings on the step entity for downstream resolution (Phase 2).
            if !citations.is_empty() {
                step_attrs.insert(
                    "citations".to_string(),
                    serde_json::Value::Array(
                        citations
                            .iter()
                            .map(|c| serde_json::Value::String(c.as_str().to_string()))
                            .collect(),
                    ),
                );
            }
            if let Some(old) = old_status {
                step_attrs.insert(a2a::OLD_STATUS.to_string(), Value::String(old.clone()));
            }
            doc.insert_entity(
                step_entity,
                Entity {
                    prov_type: Some(prov_type::<PlanStepEntityId>()),
                    attributes: step_attrs,
                },
            );
        }
        ProvEventData::MessageReceived {
            id,
            role,
            content,
            metadata: _,
            agent_id: _,
            citations: _,
        }
        | ProvEventData::MessageSent {
            id,
            role,
            content,
            metadata: _,
            agent_id: _,
            citations: _,
        } => {
            // Collect citations only for MessageSent — MessageReceived citations are
            // not expected in normal flows; we match them as _ above.
            let msg_citations: &[baml_rt_core::Citation] = match event.data() {
                ProvEventData::MessageSent { citations, .. } => citations,
                _ => &[],
            };
            let canonical_role = canonical_message_role(event, role)?;
            let message_id = message_entity_id(event.context_id(), id);
            let mut message_attrs = base_attrs(event);
            message_attrs.insert(
                a2a::MESSAGE_ID.to_string(),
                Value::String(id.as_str().to_string()),
            );
            message_attrs.insert(
                a2a::ROLE.to_string(),
                Value::String(canonical_role.to_string()),
            );
            let content_values: Vec<Value> = content
                .iter()
                .map(|line| Value::String(line.clone()))
                .collect();
            message_attrs.insert(a2a::CONTENT.to_string(), Value::Array(content_values));

            let direction = if matches!(event.data(), ProvEventData::MessageReceived { .. }) {
                message_directions::RECEIVED
            } else {
                message_directions::SENT
            };
            message_attrs.insert(
                a2a::DIRECTION.to_string(),
                Value::String(direction.to_string()),
            );

            doc.insert_entity(
                message_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<MessageEntityId>()),
                    attributes: message_attrs,
                },
            );

            let processing_id = message_processing_activity_id(event.context_id(), id);
            let mut processing_attrs = base_attrs(event);
            processing_attrs.insert(
                a2a::MESSAGE_ID.to_string(),
                Value::String(id.as_str().to_string()),
            );
            processing_attrs.insert(
                a2a::DIRECTION.to_string(),
                Value::String(direction.to_string()),
            );
            processing_attrs.insert(
                a2a::ROLE.to_string(),
                Value::String(canonical_role.to_string()),
            );
            doc.insert_activity(
                processing_id.clone(),
                Activity {
                    start_time_ms: Some(event.timestamp_ms()),
                    end_time_ms: None,
                    prov_type: Some(prov_type::<MessageProcessingActivityId>()),
                    attributes: processing_attrs,
                },
            );

            // A message is always sent to/from an agent; agent_id is required on the event.
            let agent_id_for_processing = event.message_agent_id().cloned();
            if let Some(agent_id) = agent_id_for_processing.as_ref() {
                let executing_agent_id =
                    get_agent_runtime_instance(&doc, agent_id, &mut agent_labels)?;
                insert_was_associated_with(
                    &mut doc,
                    processing_id.clone(),
                    executing_agent_id,
                    Some(prov_roles::EXECUTING_AGENT.to_string()),
                );
            }

            let invoking_agent_id = runner_runtime_instance_id();
            ensure_runner_runtime_instance(&mut doc);
            insert_was_associated_with(
                &mut doc,
                processing_id.clone(),
                invoking_agent_id,
                Some(prov_roles::INVOKING_AGENT.to_string()),
            );

            match event.data() {
                ProvEventData::MessageReceived { .. } => {
                    insert_used(
                        &mut doc,
                        processing_id.clone(),
                        message_id.clone(),
                        Some(a2a_roles::INPUT_MESSAGE.to_string()),
                    );
                }
                ProvEventData::MessageSent { .. } => {
                    insert_was_generated_by(
                        &mut doc,
                        ProvNodeRef::Entity(message_id.clone()),
                        processing_id.clone(),
                        Some(event.timestamp_ms()),
                    );
                }
                _ => {}
            }

            // Write CITED graph edges for each citation produced by the agent in this
            // message. The `to_id` is a deterministic citation-stub entity whose ID encodes
            // the context + raw citation string, so the edge is self-consistent without
            // needing a live RefTable. The `raw` edge attribute carries the original
            // citation string (#N, @N, …) for graph-traversal consumers.
            for citation in msg_citations {
                let raw = citation.as_str();
                // Deterministic stub entity ID so re-normalizing the same event is idempotent.
                let stub_id = ProvEntityId::from_write_time_node_id(format!(
                    "citation-ref:{}:{raw}",
                    event.context_id().as_str()
                ));
                // Upsert a lightweight stub entity so the edge target is valid.
                doc.insert_entity(
                    stub_id.clone(),
                    Entity {
                        prov_type: Some("CitationRef".to_string()),
                        attributes: {
                            let mut a = HashMap::new();
                            a.insert("raw".to_string(), Value::String(raw.to_string()));
                            a.insert(
                                a2a::CONTEXT_ID.to_string(),
                                Value::String(event.context_id().as_str().to_string()),
                            );
                            a
                        },
                    },
                );
                let mut edge_attrs = HashMap::new();
                edge_attrs.insert(
                    "prov_type".to_string(),
                    Value::String(a2a_relation_types::CITED.to_string()),
                );
                edge_attrs.insert("raw".to_string(), Value::String(raw.to_string()));
                derived_relations.push(A2aDerivedRelation {
                    relation: A2aRelationType::CitedSource,
                    from: ProvNodeRef::Entity(message_id.clone()),
                    to: ProvNodeRef::Entity(stub_id),
                    attributes: edge_attrs,
                });
            }

            if let Some(task_id) = event.task_id() {
                let task_entity = ensure_task_entity(&mut doc, task_id);

                let task_execution = ensure_task_execution_activity(
                    &mut doc,
                    task_id,
                    event.context_id(),
                    None,
                    None,
                    &mut agent_labels,
                    event.message_agent_id(),
                    context,
                )?;
                if matches!(event.data(), ProvEventData::MessageReceived { .. }) {
                    insert_used(
                        &mut doc,
                        task_execution.clone(),
                        message_id.clone(),
                        Some(a2a_roles::INPUT_MESSAGE.to_string()),
                    );
                }
                let mut attrs = derived_attrs(event);
                attrs.insert(
                    a2a::DIRECTION.to_string(),
                    Value::String(direction.to_string()),
                );
                derived_relations.push(A2aDerivedRelation {
                    relation: A2aRelationType::TaskHasMessage,
                    from: ProvNodeRef::Entity(task_entity),
                    to: ProvNodeRef::Entity(message_id),
                    attributes: attrs,
                });
            }
        }
        ProvEventData::ToolSessionStep {
            tool_name,
            session_id,
            op_kind,
            header,
            archive_ref,
            grep,
            offset: _,
            limit: _,
            scope,
            informed_by_tool_activity_anchor,
        } => {
            let session_scope_task: Option<&TaskId> = match scope {
                CallScope::Task { task_id } => Some(task_id),
                CallScope::Message { .. } => None,
            };
            let step_id = ProvEntityId::derived::<SessionStepEntityId>(SessionStepEntityInput {
                event_anchor: event.id().as_str(),
            });
            let mut attrs = base_attrs(event);
            if let Some(tid) = session_scope_task {
                attrs.insert(
                    a2a::TASK_ID.to_string(),
                    Value::String(tid.as_str().to_string()),
                );
            }
            attrs.insert(a2a::TOOL_NAME.to_string(), Value::String(tool_name.clone()));
            attrs.insert("session_id".to_string(), Value::String(session_id.clone()));
            attrs.insert("op_kind".to_string(), Value::String(op_kind.clone()));
            if let Some(h) = header {
                attrs.insert("header".to_string(), Value::String(h.clone()));
            }
            if let Some(r) = archive_ref {
                attrs.insert("archive_ref".to_string(), Value::String(r.clone()));
            }
            if let Some(g) = grep {
                attrs.insert("grep".to_string(), Value::String(g.clone()));
            }
            if let Some(a) = informed_by_tool_activity_anchor {
                attrs.insert(
                    "informed_by_tool_activity_anchor".to_string(),
                    Value::String(a.clone()),
                );
            }
            let entity_id = step_id;
            doc.insert_entity(
                entity_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<SessionStepEntityId>()),
                    attributes: attrs,
                },
            );
            if let Some(tid) = session_scope_task {
                let task_entity = ensure_task_entity(&mut doc, tid);
                let mut edge_attrs = derived_attrs(event);
                edge_attrs.insert(
                    a2a::TASK_ID.to_string(),
                    Value::String(tid.as_str().to_string()),
                );
                derived_relations.push(A2aDerivedRelation {
                    relation: A2aRelationType::TaskHasSessionStep,
                    from: ProvNodeRef::Entity(task_entity),
                    to: ProvNodeRef::Entity(entity_id.clone()),
                    attributes: edge_attrs,
                });
            }
            if op_kind == "send_done" {
                let and = informed_by_tool_activity_anchor
                    .as_ref()
                    .expect("SendDone must always carry informed_by anchor");
                if let Some(tool_act) = call_state.tool_call_anchor_to_activity.get(and.as_str()) {
                    let mut edge_attrs = HashMap::new();
                    edge_attrs.insert(
                        "prov_type".to_string(),
                        Value::String(a2a_relation_types::INFORMED_BY_TOOL_INVOCATION.to_string()),
                    );
                    derived_relations.push(A2aDerivedRelation {
                        relation: A2aRelationType::InformedByToolInvocation,
                        from: ProvNodeRef::Entity(entity_id),
                        to: ProvNodeRef::Activity(tool_act.clone()),
                        attributes: edge_attrs,
                    });
                } else {
                    tracing::warn!(
                        anchor = %and,
                        "SendDone informed_by anchor not found in call_state; ToolCallCompleted may have been normalized in a different session"
                    );
                }
            }
        }
    }

    Ok(NormalizedProv {
        document: doc,
        derived_relations,
        agent_labels,
    })
}

pub fn validate_event(event: &ProvEvent) -> Result<()> {
    match event.data() {
        ProvEventData::LlmCallStarted {
            scope, metadata, ..
        }
        | ProvEventData::LlmCallCompleted {
            scope, metadata, ..
        } => {
            validate_call_scope(event, scope, "llm call")?;
            validate_required_call_metadata(event, scope, metadata, "llm call")?;
        }
        ProvEventData::ToolCallStarted {
            scope, metadata, ..
        }
        | ProvEventData::ToolCallCompleted {
            scope, metadata, ..
        } => {
            validate_call_scope(event, scope, "tool call")?;
            validate_required_call_metadata(event, scope, metadata, "tool call")?;
        }
        ProvEventData::PromptRejected { scope, .. } => {
            validate_call_scope(event, scope, "prompt rejected")?;
        }
        ProvEventData::TaskExists {
            task_id,
            context_id,
        } => {
            validate_task_scoped_event(event, task_id, "TaskExists")?;
            if event.context_id() != context_id {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason: "TaskExists context_id must match event context_id".to_string(),
                });
            }
        }
        ProvEventData::TaskExecutionStarted {
            task_id,
            context_id,
            ..
        } => {
            validate_task_scoped_event(event, task_id, "TaskExecutionStarted")?;
            if event.context_id() != context_id {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason: "TaskExecutionStarted context_id must match event context_id"
                        .to_string(),
                });
            }
        }
        ProvEventData::TaskExecutionEnded {
            task_id,
            context_id,
        } => {
            validate_task_scoped_event(event, task_id, "TaskExecutionEnded")?;
            if event.context_id() != context_id {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason: "TaskExecutionEnded context_id must match event context_id".to_string(),
                });
            }
        }
        ProvEventData::IntentResolved { task_id, .. }
        | ProvEventData::PlanGenerated { task_id, .. }
        | ProvEventData::PlanStepStatusChanged { task_id, .. } => {
            validate_task_scoped_event(event, task_id, "Plan/Intent event")?;
        }
        ProvEventData::MessageReceived { role, content, .. }
        | ProvEventData::MessageSent { role, content, .. } => {
            canonical_message_role(event, role)?;
            if content.iter().all(|line| line.trim().is_empty()) {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason: "message content must include at least one non-empty text part"
                        .to_string(),
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_task_scoped_event(event: &ProvEvent, task_id: &TaskId, event_kind: &str) -> Result<()> {
    let event_id = event.id().as_str().to_string();
    match event {
        ProvEvent::Global(_) => Err(ProvenanceError::InvalidEvent {
            activity_anchor: event_id,
            reason: format!("{event_kind} must be task-scoped (event is global)"),
        }),
        ProvEvent::AgentBooted(_) => Err(ProvenanceError::InvalidEvent {
            activity_anchor: event_id,
            reason: format!("{event_kind} cannot be AgentBooted (has no context)"),
        }),
        ProvEvent::Task(e) if &e.task_id == task_id => Ok(()),
        ProvEvent::Task(_) => Err(ProvenanceError::InvalidEvent {
            activity_anchor: event_id,
            reason: format!("{event_kind} task_id must match event task_id"),
        }),
    }
}

fn validate_call_scope(event: &ProvEvent, scope: &CallScope, call_kind: &str) -> Result<()> {
    let event_id = event.id().as_str().to_string();
    match (event, scope) {
        (ProvEvent::Global(_), CallScope::Message { .. }) => Ok(()),
        (ProvEvent::Global(_), CallScope::Task { .. }) => Err(ProvenanceError::InvalidEvent {
            activity_anchor: event_id.clone(),
            reason: format!("{call_kind} is task-scoped but event is global"),
        }),
        (ProvEvent::Task(_event), CallScope::Message { .. }) => {
            Err(ProvenanceError::InvalidEvent {
                activity_anchor: event_id.clone(),
                reason: format!("{call_kind} is message-scoped but event is task-scoped"),
            })
        }
        (ProvEvent::Task(event), CallScope::Task { task_id }) => {
            if task_id == &event.task_id {
                Ok(())
            } else {
                Err(ProvenanceError::InvalidEvent {
                    activity_anchor: event_id,
                    reason: format!("{call_kind} task_id does not match event task_id"),
                })
            }
        }
        (ProvEvent::AgentBooted(_), _) => Err(ProvenanceError::InvalidEvent {
            activity_anchor: event_id,
            reason: format!("{call_kind} cannot be AgentBooted (has no call scope)"),
        }),
    }
}

/// Boundary validation point for call metadata (LLM calls, tool calls).
///
/// This is the **single** place where untyped `metadata` JSON from EffectEvents is
/// validated and rejected if structurally incomplete. After this function succeeds,
/// downstream normalisation code may assume `agent_id` (and `message_id` for
/// message-scoped calls) are present and well-formed — no re-parsing needed.
fn validate_required_call_metadata(
    event: &ProvEvent,
    scope: &CallScope,
    metadata: &Value,
    call_kind: &str,
) -> Result<()> {
    let event_id = event.id().as_str().to_string();
    let obj = metadata
        .as_object()
        .ok_or_else(|| ProvenanceError::MissingField {
            activity_anchor: event_id.clone(),
            field: "metadata".to_string(),
        })?;

    let agent_id = extract_agent_id(metadata).ok_or_else(|| ProvenanceError::MissingField {
        activity_anchor: event_id.clone(),
        field: "metadata.agent_id".to_string(),
    })?;
    parse_agent_id(event, agent_id)?;

    if matches!(scope, CallScope::Message { .. })
        && obj.get("message_id").and_then(|v| v.as_str()).is_none()
    {
        return Err(ProvenanceError::MissingField {
            activity_anchor: event_id,
            field: format!("metadata.message_id ({call_kind})"),
        });
    }

    Ok(())
}

fn base_attrs(event: &ProvEvent) -> HashMap<String, Value> {
    let mut attrs = HashMap::new();
    if let Some(ctx) = event.context_id_opt() {
        attrs.insert(
            a2a::CONTEXT_ID.to_string(),
            Value::String(ctx.as_str().to_string()),
        );
    }
    if let Some(task_id) = event.task_id() {
        attrs.insert(
            a2a::TASK_ID.to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
    let anchor = event.id().as_str().to_string();
    if let Some(order) = anchor
        .strip_prefix("prov-")
        .and_then(|s| s.parse::<u64>().ok())
    {
        attrs.insert(a2a::EVENT_ORDER.to_string(), Value::Number(order.into()));
    }
    attrs.insert(a2a::ACTIVITY_ANCHOR.to_string(), Value::String(anchor));
    attrs
}

fn derived_attrs(event: &ProvEvent) -> HashMap<String, Value> {
    let mut attrs = HashMap::new();
    if let Some(ctx) = event.context_id_opt() {
        attrs.insert(
            a2a::CONTEXT_ID.to_string(),
            Value::String(ctx.as_str().to_string()),
        );
    }
    if let Some(task_id) = event.task_id() {
        attrs.insert(
            a2a::TASK_ID.to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
    attrs.insert(
        a2a::TIMESTAMP_MS.to_string(),
        Value::Number(event.timestamp_ms().into()),
    );
    let anchor = event.id().as_str();
    if let Some(order) = anchor
        .strip_prefix("prov-")
        .and_then(|s| s.parse::<u64>().ok())
    {
        attrs.insert(a2a::EVENT_ORDER.to_string(), Value::Number(order.into()));
    }
    attrs
}

fn ensure_task_entity(doc: &mut ProvDocument, task_id: &TaskId) -> ProvEntityId {
    let id = task_entity_id(task_id);
    let mut attrs = doc
        .entity(&id)
        .map(|entity| entity.attributes.clone())
        .unwrap_or_default();
    attrs.insert(
        a2a::TASK_ID.to_string(),
        Value::String(task_id.as_str().to_string()),
    );
    doc.insert_entity(
        id.clone(),
        Entity {
            prov_type: Some(prov_type::<TaskEntityId>()),
            attributes: attrs,
        },
    );
    id
}

#[allow(clippy::too_many_arguments)]
fn ensure_task_execution_activity(
    doc: &mut ProvDocument,
    task_id: &TaskId,
    context_id: &ContextId,
    start_time_ms: Option<u64>,
    end_time_ms: Option<u64>,
    agent_labels: &mut HashMap<String, String>,
    agent_id_override: Option<&AgentId>,
    context: Option<&NormalizeContext>,
) -> Result<ProvActivityId> {
    let id = task_execution_activity_id(task_id);
    let (mut attrs, existing_start, existing_end) = if let Some(activity) = doc.activity(&id) {
        (
            activity.attributes.clone(),
            activity.start_time_ms,
            activity.end_time_ms,
        )
    } else {
        (HashMap::new(), None, None)
    };
    attrs.insert(
        a2a::TASK_ID.to_string(),
        Value::String(task_id.as_str().to_string()),
    );
    attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(context_id.as_str().to_string()),
    );
    let agent_id = agent_id_override
        .or_else(|| context.and_then(|c| c.task_agent_id.as_ref()))
        .cloned();

    // Look up agent_type from runtime instance agent for display purposes
    if let Some(ref agent_id) = agent_id {
        let agent_instance_id = agent_runtime_instance_id(agent_id);
        if let Some(agent) = doc
            .agent(&agent_instance_id)
            .and_then(|agent| agent.attributes.get(a2a::AGENT_TYPE))
            .and_then(|v| v.as_str())
        {
            attrs.insert(
                a2a::AGENT_TYPE.to_string(),
                Value::String(agent.to_string()),
            );
        }
    }

    let start_time_ms = existing_start.or(start_time_ms);
    let end_time_ms = existing_end.or(end_time_ms);
    doc.insert_activity(
        id.clone(),
        Activity {
            start_time_ms,
            end_time_ms,
            prov_type: Some(prov_type::<TaskExecutionActivityId>()),
            attributes: attrs,
        },
    );
    // Associate with agent if available - if not, association will be added when TaskExecutionStarted is processed
    associate_task_execution_agents(doc, &id, agent_id.as_ref(), agent_labels)?;
    Ok(id)
}

fn insert_used(
    doc: &mut ProvDocument,
    activity: ProvActivityId,
    entity: ProvEntityId,
    role: Option<String>,
) {
    let id = doc.blank_node_id("u");
    doc.insert_used(
        id,
        Used {
            activity,
            entity,
            role,
        },
    );
}

fn insert_was_generated_by(
    doc: &mut ProvDocument,
    entity: ProvNodeRef,
    activity: ProvActivityId,
    time_ms: Option<u64>,
) {
    let id = doc.blank_node_id("g");
    doc.insert_was_generated_by(
        id,
        WasGeneratedBy {
            entity,
            activity,
            time_ms,
        },
    );
}

fn insert_qualified_generation(
    doc: &mut ProvDocument,
    entity: ProvNodeRef,
    activity: ProvActivityId,
    time_ms: Option<u64>,
) {
    let id = doc.blank_node_id("gen");
    doc.insert_qualified_generation(
        id,
        QualifiedGeneration {
            entity,
            activity,
            time_ms,
        },
    );
}

fn insert_was_associated_with(
    doc: &mut ProvDocument,
    activity: ProvActivityId,
    agent: ProvAgentId,
    role: Option<String>,
) {
    let id = doc.blank_node_id("assoc");
    doc.insert_was_associated_with(
        id,
        WasAssociatedWith {
            activity,
            agent,
            role,
        },
    );
}

fn insert_was_derived_from(
    doc: &mut ProvDocument,
    generated_entity: ProvEntityId,
    used_entity: ProvEntityId,
    activity: Option<ProvActivityId>,
    prov_type: Option<String>,
) {
    let id = doc.blank_node_id("d");
    doc.insert_was_derived_from(
        id,
        WasDerivedFrom {
            generated_entity,
            used_entity,
            activity,
            prov_type,
        },
    );
}

fn upsert_failure_classification(
    doc: &mut ProvDocument,
    event: &ProvEvent,
    target: FailureClassificationTarget,
    resolution: &FailureResolution,
) {
    // `PromptRejected` and similar events reference a failed call activity from a prior batch;
    // that activity may be absent from this document. Insert a minimal phantom activity so
    // `build_label_maps` records the correct `from_label` (LlmCall / ToolCall) on the FC edge.
    if doc.activity(&target.failed_activity_id).is_none() {
        let prov_type_for_failed = if target.call_kind == "tool_call" {
            prov_type::<ToolCallActivityId>()
        } else {
            prov_type::<LlmCallActivityId>()
        };
        doc.insert_activity(
            target.failed_activity_id.clone(),
            Activity {
                start_time_ms: None,
                end_time_ms: None,
                prov_type: Some(prov_type_for_failed),
                attributes: HashMap::new(),
            },
        );
    }

    let scope_key_str = target.scope_key.to_string();
    let classify_activity_id =
        failure_classification_activity_id(target.call_kind, &scope_key_str, target.ordinal);
    let mut classify_attrs = base_attrs(event);
    classify_attrs.insert(
        a2a::FAILURE_EVIDENCE.to_string(),
        Value::String(resolution.evidence.clone()),
    );
    doc.insert_activity(
        classify_activity_id.clone(),
        Activity {
            start_time_ms: Some(event.timestamp_ms()),
            end_time_ms: Some(event.timestamp_ms()),
            prov_type: Some(prov_type::<FailureClassificationActivityId>()),
            attributes: classify_attrs,
        },
    );

    let classify_entity_id =
        failure_classification_entity_id(target.call_kind, &scope_key_str, target.ordinal);
    let mut classify_entity_attrs = base_attrs(event);
    classify_entity_attrs.insert(
        a2a::FAILURE_CLASS.to_string(),
        Value::String(resolution.class.clone()),
    );
    classify_entity_attrs.insert(
        a2a::FAILURE_EVIDENCE.to_string(),
        Value::String(resolution.evidence.clone()),
    );
    if let Some(code) = &resolution.code {
        classify_entity_attrs.insert(a2a::FAILURE_CODE.to_string(), Value::String(code.clone()));
    }
    doc.insert_entity(
        classify_entity_id.clone(),
        Entity {
            prov_type: Some(prov_type::<FailureClassificationEntityId>()),
            attributes: classify_entity_attrs,
        },
    );
    insert_was_generated_by(
        doc,
        ProvNodeRef::Entity(classify_entity_id.clone()),
        classify_activity_id.clone(),
        Some(event.timestamp_ms()),
    );
    insert_used(
        doc,
        target.failed_activity_id,
        classify_entity_id,
        Some(a2a_roles::FAILURE_CLASSIFICATION.to_string()),
    );
    if let Some(evidence) = target.evidence_entity {
        insert_used(
            doc,
            classify_activity_id,
            evidence,
            Some(a2a_roles::FAILURE_EVIDENCE.to_string()),
        );
    }
}

/// LLM call activity id: derived from `ActivityAnchorId` to ensure per-call uniqueness.
/// LLM call activity id: deterministic composite from (scope_key, ordinal).
fn llm_activity_id(scope_key: &str, ordinal: u64) -> ProvActivityId {
    ProvActivityId::derived::<LlmCallActivityId>(LlmCallActivityInput { scope_key, ordinal })
}

/// Tool call activity id: deterministic composite from (scope_key, ordinal).
fn tool_activity_id(scope_key: &str, ordinal: u64) -> ProvActivityId {
    ProvActivityId::derived::<ToolCallActivityId>(ToolCallActivityInput { scope_key, ordinal })
}

fn llm_prompt_entity_id(scope_key: &str, ordinal: u64) -> ProvEntityId {
    ProvEntityId::derived::<LlmPromptEntityId>(LlmPromptEntityInput { scope_key, ordinal })
}

fn prompt_rejected_activity_id(scope_key: &str, ordinal: u64) -> ProvActivityId {
    ProvActivityId::derived::<PromptRejectedActivityId>(PromptRejectedActivityInput {
        scope_key,
        ordinal,
    })
}

fn failure_classification_activity_id(
    call_kind: &str,
    scope_key: &str,
    ordinal: u64,
) -> ProvActivityId {
    ProvActivityId::derived::<FailureClassificationActivityId>(FailureClassificationActivityInput {
        call_kind,
        scope_key,
        ordinal,
    })
}

fn failure_classification_entity_id(
    call_kind: &str,
    scope_key: &str,
    ordinal: u64,
) -> ProvEntityId {
    ProvEntityId::derived::<FailureClassificationEntityId>(FailureClassificationEntityInput {
        call_kind,
        scope_key,
        ordinal,
    })
}

fn tool_args_entity_id(scope_key: &str, ordinal: u64) -> ProvEntityId {
    ProvEntityId::derived::<ToolArgsEntityId>(ToolArgsEntityInput { scope_key, ordinal })
}

fn delegation_target_entity_id(scope_key: &str, ordinal: u64) -> ProvEntityId {
    ProvEntityId::derived::<DelegationTargetEntityId>(DelegationTargetEntityInput {
        scope_key,
        ordinal,
    })
}

/// Task entity id: derived from `TaskId` to provide stable task identity.
fn task_entity_id(task_id: &TaskId) -> ProvEntityId {
    ProvEntityId::derived::<TaskEntityId>(TaskEntityInput { task_id })
}

/// Public helper so the store can build the task entity id string for graph lookups.
pub fn task_entity_id_string(task_id: &TaskId) -> String {
    task_entity_id(task_id).into_string()
}

/// Task execution activity id: derived from `TaskId` to group task execution edges.
fn task_execution_activity_id(task_id: &TaskId) -> ProvActivityId {
    ProvActivityId::derived::<TaskExecutionActivityId>(TaskExecutionActivityInput { task_id })
}

/// Agent runtime instance entity id: derived from `AgentId`.
fn agent_runtime_instance_id(agent_id: &AgentId) -> ProvAgentId {
    ProvAgentId::derived::<AgentRuntimeInstanceId>(AgentRuntimeInstanceInput { agent_id })
}

/// Archive entity id: derived from package identity (name@version or hash).
fn archive_entity_id(archive_path: &str) -> ProvEntityId {
    ProvEntityId::derived::<ArchiveEntityId>(ArchiveEntityInput { archive_path })
}

/// Agent boot activity id: derived from `AgentId` (one boot per runtime instance).
fn boot_activity_id(agent_id: &AgentId) -> ProvActivityId {
    ProvActivityId::derived::<AgentBootActivityId>(AgentBootActivityInput { agent_id })
}

/// Runner runtime instance entity id: constant control plane identity.
fn runner_runtime_instance_id() -> ProvAgentId {
    ProvAgentId::constant::<RunnerRuntimeInstanceId>()
}

/// Look up an agent runtime instance in the document.
/// If the instance is not in the document, add it to agent_labels so the graph build can create it
/// (either from agent_registry / AgentBooted, or on first reference e.g. task store operations).
fn get_agent_runtime_instance(
    doc: &ProvDocument,
    agent_id: &AgentId,
    agent_labels: &mut HashMap<String, String>,
) -> Result<ProvAgentId> {
    let instance_id = agent_runtime_instance_id(agent_id);
    if doc.agent(&instance_id).is_some() {
        Ok(instance_id)
    } else {
        agent_labels
            .entry(instance_id.as_str().to_string())
            .or_insert_with(|| "AgentRuntimeInstance".to_string());
        Ok(instance_id)
    }
}

fn ensure_runner_runtime_instance(doc: &mut ProvDocument) {
    let id = runner_runtime_instance_id();
    if doc.agent(&id).is_none() {
        let mut attrs = HashMap::new();
        attrs.insert(
            a2a::AGENT_TYPE.to_string(),
            Value::String(agent_types::RUNNER.to_string()),
        );
        doc.insert_agent(
            id,
            Agent {
                prov_type: Some(prov_type::<RunnerRuntimeInstanceId>()),
                attributes: attrs,
            },
        );
    }
}

/// Message entity id: derived from `(ContextId, MessageId)`.
fn message_entity_id(context_id: &ContextId, message_id: &MessageId) -> ProvEntityId {
    ProvEntityId::derived::<MessageEntityId>(MessageEntityInput {
        context_id,
        message_id,
    })
}

/// Message processing activity id: derived from `(ContextId, MessageId)`.
fn message_processing_activity_id(
    context_id: &ContextId,
    message_id: &MessageId,
) -> ProvActivityId {
    ProvActivityId::derived::<MessageProcessingActivityId>(MessageProcessingActivityInput {
        context_id,
        message_id,
    })
}

fn ensure_message_processing_activity(
    doc: &mut ProvDocument,
    context_id: &ContextId,
    message_id: &MessageId,
) -> ProvActivityId {
    let id = message_processing_activity_id(context_id, message_id);
    let mut attrs = doc
        .activity(&id)
        .map(|activity| activity.attributes.clone())
        .unwrap_or_default();
    attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(context_id.as_str().to_string()),
    );
    attrs.insert(
        a2a::MESSAGE_ID.to_string(),
        Value::String(message_id.as_str().to_string()),
    );
    doc.insert_activity(
        id.clone(),
        Activity {
            start_time_ms: None,
            end_time_ms: None,
            prov_type: Some(prov_type::<MessageProcessingActivityId>()),
            attributes: attrs,
        },
    );
    id
}

fn attach_message_context(
    doc: &mut ProvDocument,
    event: &ProvEvent,
    activity_id: &ProvActivityId,
    message_id: &MessageId,
    derived_relations: &mut Vec<A2aDerivedRelation>,
) {
    let message_entity_id = message_entity_id(event.context_id(), message_id);
    // Placeholder only: do not stamp role/content/activity_anchor from call events.
    // MessageReceived/MessageSent owns semantic message fields.
    let mut message_attrs = doc
        .entity(&message_entity_id)
        .map(|entity| entity.attributes.clone())
        .unwrap_or_default();
    message_attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(event.context_id().as_str().to_string()),
    );
    message_attrs.insert(
        a2a::MESSAGE_ID.to_string(),
        Value::String(message_id.as_str().to_string()),
    );
    doc.insert_entity(
        message_entity_id.clone(),
        Entity {
            prov_type: Some(prov_type::<MessageEntityId>()),
            attributes: message_attrs,
        },
    );
    insert_used(
        doc,
        activity_id.clone(),
        message_entity_id,
        Some(a2a_roles::INPUT_MESSAGE.to_string()),
    );
    let processing_id = ensure_message_processing_activity(doc, event.context_id(), message_id);
    derived_relations.push(A2aDerivedRelation {
        relation: A2aRelationType::MessageCall,
        from: ProvNodeRef::Activity(processing_id),
        to: ProvNodeRef::Activity(activity_id.clone()),
        attributes: derived_attrs(event),
    });
}

fn scoped_message_id(scope: &CallScope, metadata: &Value) -> Option<MessageId> {
    match scope {
        CallScope::Message { message_id } => Some(message_id.clone()),
        CallScope::Task { .. } => metadata
            .get("message_id")
            .or_else(|| metadata.get(a2a::MESSAGE_ID))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| MessageId::from_external(ExternalId::new(value.to_string()))),
    }
}

fn attach_task_call_context(
    doc: &mut ProvDocument,
    event: &ProvEvent,
    activity_id: &ProvActivityId,
    derived_relations: &mut Vec<A2aDerivedRelation>,
    agent_labels: &mut HashMap<String, String>,
    context: Option<&NormalizeContext>,
) -> Result<()> {
    let Some(task_id) = event.task_id() else {
        return Ok(());
    };
    let Some(ctx) = event.context_id_opt() else {
        return Ok(());
    };

    // Try to get agent_id from event metadata (for LLM/Tool calls)
    let agent_id_from_metadata = match event.data() {
        ProvEventData::LlmCallStarted { metadata, .. }
        | ProvEventData::LlmCallCompleted { metadata, .. }
        | ProvEventData::ToolCallStarted { metadata, .. }
        | ProvEventData::ToolCallCompleted { metadata, .. } => metadata
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| parse_agent_id(event, s))
            .transpose()?,
        _ => None,
    };

    let _task_entity = ensure_task_entity(doc, task_id);

    let task_execution =
        ensure_task_execution_activity(doc, task_id, ctx, None, None, agent_labels, None, context)?;
    associate_call_with_agent(
        doc,
        activity_id,
        agent_id_from_metadata
            .as_ref()
            .or_else(|| context.and_then(|c| c.task_agent_id.as_ref())),
        agent_labels,
    )?;
    derived_relations.push(A2aDerivedRelation {
        relation: A2aRelationType::TaskCall,
        from: ProvNodeRef::Activity(task_execution),
        to: ProvNodeRef::Activity(activity_id.clone()),
        attributes: derived_attrs(event),
    });
    Ok(())
}

fn associate_task_execution_agents(
    doc: &mut ProvDocument,
    task_execution: &ProvActivityId,
    agent_id: Option<&AgentId>,
    agent_labels: &mut HashMap<String, String>,
) -> Result<()> {
    let Some(agent_id) = agent_id else {
        return Ok(());
    };

    let executing_agent_id = get_agent_runtime_instance(doc, agent_id, agent_labels)?;
    insert_was_associated_with(
        doc,
        task_execution.clone(),
        executing_agent_id,
        Some(prov_roles::EXECUTING_AGENT.to_string()),
    );

    let invoking_agent_id = runner_runtime_instance_id();
    ensure_runner_runtime_instance(doc);
    insert_was_associated_with(
        doc,
        task_execution.clone(),
        invoking_agent_id,
        Some(prov_roles::INVOKING_AGENT.to_string()),
    );
    Ok(())
}

fn associate_call_with_agent(
    doc: &mut ProvDocument,
    activity_id: &ProvActivityId,
    agent_id: Option<&AgentId>,
    agent_labels: &mut HashMap<String, String>,
) -> Result<()> {
    // If agent_id is available, associate the call with the agent
    // If not, the association will be added when TaskExecutionStarted is processed
    if let Some(agent_id) = agent_id {
        let executing_agent_id = get_agent_runtime_instance(doc, agent_id, agent_labels)?;
        insert_was_associated_with(
            doc,
            activity_id.clone(),
            executing_agent_id,
            Some(prov_roles::EXECUTING_AGENT.to_string()),
        );
    }
    Ok(())
}

fn task_state_entity_id(task_id: &TaskId, status: &str) -> ProvEntityId {
    ProvEntityId::derived::<TaskStateEntityId>(TaskStateEntityInput { task_id, status })
}

fn intent_entity_id(task_id: &TaskId, intent_id: &IntentId) -> ProvEntityId {
    ProvEntityId::derived::<IntentEntityId>(IntentEntityInput {
        task_id,
        intent_id: intent_id.as_str(),
    })
}

fn plan_entity_id(task_id: &TaskId, plan_id: &PlanId) -> ProvEntityId {
    ProvEntityId::derived::<PlanEntityId>(PlanEntityInput {
        task_id,
        plan_id: plan_id.as_str(),
    })
}

/// Public helper so the store can build the plan entity id string for graph lookups.
pub fn plan_entity_id_string(task_id: &TaskId, plan_id: &str) -> String {
    ProvEntityId::derived::<PlanEntityId>(PlanEntityInput { task_id, plan_id }).into_string()
}

fn plan_step_entity_id(task_id: &TaskId, plan_id: &PlanId, step_id: &PlanStepId) -> ProvEntityId {
    ProvEntityId::derived::<PlanStepEntityId>(PlanStepEntityInput {
        task_id,
        plan_id: plan_id.as_str(),
        step_id: step_id.as_str(),
    })
}

fn artifact_entity_id(
    task_id: &TaskId,
    artifact_id: &Option<ArtifactId>,
    artifact_type: &Option<String>,
    activity_anchor: &ActivityAnchorId,
) -> ProvEntityId {
    let identity = if let Some(artifact_id) = artifact_id {
        ArtifactIdentity::ById(artifact_id)
    } else if let Some(artifact_type) = artifact_type {
        ArtifactIdentity::ByType {
            task_id,
            artifact_type,
        }
    } else {
        ArtifactIdentity::ByActivityAnchor {
            task_id,
            activity_anchor,
        }
    };
    match identity {
        ArtifactIdentity::ById(artifact_id) => {
            ProvEntityId::derived::<ArtifactByIdEntityId>(ArtifactByIdEntityInput { artifact_id })
        }
        ArtifactIdentity::ByType {
            task_id,
            artifact_type,
        } => ProvEntityId::derived::<ArtifactByTypeEntityId>(ArtifactByTypeEntityInput {
            task_id,
            artifact_type,
        }),
        ArtifactIdentity::ByActivityAnchor {
            task_id,
            activity_anchor,
        } => ProvEntityId::derived::<ArtifactByActivityAnchorEntityId>(
            ArtifactByActivityAnchorEntityInput {
                task_id,
                activity_anchor,
            },
        ),
    }
}

fn is_terminal_status(status: &str) -> bool {
    let normalized = status.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "completed" | "failed" | "cancelled" | "canceled"
    )
}
