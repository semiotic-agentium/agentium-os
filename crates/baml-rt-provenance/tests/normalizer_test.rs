use baml_rt_core::{
    Citation, Outcome,
    bus::PlanningSupersessionKind,
    ids::{AgentId, ContextId, ExternalId, IntentId, MessageId, PlanId, TaskId, UuidId},
};
use baml_rt_provenance::{
    A2aRelationType, DefaultProvNormalizer, LlmUsage, ProvEvent, ProvNormalizer, normalize_event,
    vocabulary::{a2a_roles, a2a_types, semantic_labels},
};

#[test]
fn normalize_status_change_includes_derived_relation() {
    let event = ProvEvent::task_status_changed(
        ContextId::new(1, 1),
        TaskId::from_external(ExternalId::new("task-1")),
        Some("TASK_STATE_PENDING".to_string()),
        Some("TASK_STATE_WORKING".to_string()),
    );
    let normalized = normalize_event(&event).expect("normalize event");
    assert_eq!(normalized.document.was_derived_from().count(), 1);
    assert!(
        normalized
            .derived_relations
            .iter()
            .any(|rel| matches!(rel.relation, A2aRelationType::TaskStatusTransition))
    );
}

#[test]
fn normalize_intent_and_plan_revisions_emit_replaced_by_relations() {
    let context_id = ContextId::new(2, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-revision-1"));
    let normalizer = DefaultProvNormalizer::default();

    normalizer
        .normalize(&ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            IntentId::from("intent-v1".to_string()),
            "v1".to_string(),
            vec![Citation::try_new("#1").expect("citation")],
            None,
            None,
        ))
        .expect("intent v1");
    let intent_v2 = normalizer
        .normalize(&ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            IntentId::from("intent-v2".to_string()),
            "v2".to_string(),
            vec![Citation::try_new("#1").expect("citation")],
            None,
            None,
        ))
        .expect("intent v2");
    assert!(intent_v2.derived_relations.iter().any(|rel| {
        matches!(rel.relation, A2aRelationType::IntentReplacedBy)
            && rel
                .attributes
                .get("a2a:relation")
                .and_then(serde_json::Value::as_str)
                .is_none()
    }));

    normalizer
        .normalize(&ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            IntentId::from("intent-v2".to_string()),
            PlanId::from("plan-v1".to_string()),
            vec![],
            None,
        ))
        .expect("plan v1");
    let plan_v2 = normalizer
        .normalize(&ProvEvent::plan_generated(
            context_id,
            task_id,
            IntentId::from("intent-v2".to_string()),
            PlanId::from("plan-v2".to_string()),
            vec![],
            None,
        ))
        .expect("plan v2");
    assert!(plan_v2.derived_relations.iter().any(|rel| {
        matches!(rel.relation, A2aRelationType::PlanReplacedBy)
            && rel.relation.as_str() == semantic_labels::WAS_REPLACED_BY
    }));
}

#[test]
fn normalize_intent_and_plan_revisions_emit_refined_by_relations() {
    let context_id = ContextId::new(3, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-refine-1"));
    let normalizer = DefaultProvNormalizer::default();

    normalizer
        .normalize(&ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            IntentId::from("intent-v1".to_string()),
            "v1".to_string(),
            vec![Citation::try_new("#1").expect("citation")],
            None,
            None,
        ))
        .expect("intent v1");
    let intent_v2 = normalizer
        .normalize(&ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            IntentId::from("intent-v2".to_string()),
            "v2".to_string(),
            vec![Citation::try_new("#1").expect("citation")],
            Some(PlanningSupersessionKind::RefinedBy),
            None,
        ))
        .expect("intent v2");
    assert!(
        intent_v2
            .derived_relations
            .iter()
            .any(|rel| matches!(rel.relation, A2aRelationType::IntentRefinedBy))
    );

    normalizer
        .normalize(&ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            IntentId::from("intent-v2".to_string()),
            PlanId::from("plan-v1".to_string()),
            vec![],
            None,
        ))
        .expect("plan v1");
    let plan_v2 = normalizer
        .normalize(&ProvEvent::plan_generated(
            context_id,
            task_id,
            IntentId::from("intent-v2".to_string()),
            PlanId::from("plan-v2".to_string()),
            vec![],
            Some(PlanningSupersessionKind::RefinedBy),
        ))
        .expect("plan v2");
    assert!(
        plan_v2
            .derived_relations
            .iter()
            .any(|rel| matches!(rel.relation, A2aRelationType::PlanRefinedBy))
    );
}

#[test]
fn normalize_tool_call_completed_keeps_args_role_contract() {
    let context_id = ContextId::new(7, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-args-contract"));
    let event = ProvEvent::tool_call_completed_task(
        context_id,
        task_id,
        "support/clickup".to_string(),
        None,
        serde_json::json!({"task_id":"task-901"}),
        serde_json::json!({
            "agent_id": "00000000-0000-0000-0000-000000000001",
            "phase": "send",
            "result": {"tasks": []}
        }),
        15,
        Outcome::Success,
        None,
    );
    let normalized = normalize_event(&event).expect("normalize tool call completed");

    let has_args_role = normalized
        .document
        .used()
        .any(|(_, rel)| rel.role.as_deref() == Some(a2a_roles::ARGS));
    assert!(
        has_args_role,
        "normalized tool call must include USED relation with role a2a:args"
    );
}

#[test]
fn normalize_tool_call_completed_with_delegation_target_creates_was_delegated_to() {
    let context_id = ContextId::new(8, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-delegation"));
    let event = ProvEvent::tool_call_completed_task(
        context_id,
        task_id,
        "system/internal_a2a".to_string(),
        None,
        serde_json::json!({"text": "hello"}),
        serde_json::json!({
            "phase": "send",
            "agent_id": "00000000-0000-0000-0000-000000000001",
            "task_id": "task-delegation",
            "message_id": "msg-1",
            "result": {"chunks": []}
        }),
        100,
        Outcome::Success,
        Some("claude-session-demo".to_string()),
    );
    let normalized = normalize_event(&event).expect("normalize tool call with delegation target");

    let has_delegation_role = normalized
        .document
        .used()
        .any(|(_, rel)| rel.role.as_deref() == Some(a2a_roles::DELEGATION_TARGET));
    assert!(
        has_delegation_role,
        "normalized internal_a2a with delegation_target must include USED relation with role a2a:delegation_target"
    );

    let has_delegation_entity = normalized
        .document
        .entities()
        .any(|(id, _)| id.as_str().contains("delegation_target"));
    assert!(
        has_delegation_entity,
        "normalized internal_a2a with delegation_target must create DelegationTarget entity"
    );
}

#[test]
fn normalize_task_scoped_call_with_metadata_message_id_attaches_message_context() {
    let event = ProvEvent::llm_call_completed_task(
        ContextId::new(9, 1),
        TaskId::from_external(ExternalId::new("task-msg-link")),
        "default-client".to_string(),
        "model-x".to_string(),
        "ChooseAction".to_string(),
        serde_json::json!({"messages": []}),
        serde_json::json!({
            "agent_id": "00000000-0000-0000-0000-000000000001",
            "message_id": "cli-msg-1"
        }),
        LlmUsage::Known {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
            cached_input_tokens: None,
        },
        12,
        Outcome::Success,
    );
    let normalized = normalize_event(&event).expect("normalize task-scoped llm call");

    let has_message_call_relation = normalized.derived_relations.iter().any(|rel| {
        matches!(rel.relation, A2aRelationType::MessageCall) && rel.from.id().contains(":cli-msg-1")
    });
    assert!(
        has_message_call_relation,
        "task-scoped call with metadata.message_id must emit MessageCall derived relation"
    );
}

#[test]
fn normalize_same_message_id_in_different_contexts_produces_distinct_message_nodes() {
    let message_id = MessageId::from_external(ExternalId::new("cli-msg-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());

    let event_a = ProvEvent::message_received_global(
        ContextId::new(100, 1),
        message_id.clone(),
        "ROLE_USER".to_string(),
        vec!["hello".to_string()],
        None,
        agent_id.clone(),
        1,
    );
    let event_b = ProvEvent::message_received_global(
        ContextId::new(101, 1),
        message_id,
        "ROLE_USER".to_string(),
        vec!["hello".to_string()],
        None,
        agent_id,
        2,
    );

    let normalized_a = normalize_event(&event_a).expect("normalize context A message");
    let normalized_b = normalize_event(&event_b).expect("normalize context B message");

    let entity_a = normalized_a
        .document
        .entities()
        .map(|(id, _)| id.as_str().to_string())
        .find(|id| id.starts_with("message:"))
        .expect("context A message entity");
    let entity_b = normalized_b
        .document
        .entities()
        .map(|(id, _)| id.as_str().to_string())
        .find(|id| id.starts_with("message:"))
        .expect("context B message entity");
    assert_ne!(
        entity_a, entity_b,
        "message entity id must be context-scoped to avoid cross-context collisions"
    );

    let proc_a = normalized_a
        .document
        .activities()
        .map(|(id, _)| id.as_str().to_string())
        .find(|id| id.starts_with("message_processing:"))
        .expect("context A message processing activity");
    let proc_b = normalized_b
        .document
        .activities()
        .map(|(id, _)| id.as_str().to_string())
        .find(|id| id.starts_with("message_processing:"))
        .expect("context B message processing activity");
    assert_ne!(
        proc_a, proc_b,
        "message processing activity id must be context-scoped to avoid cross-context collisions"
    );
}

#[test]
fn normalize_message_role_aliases_to_wire_constants() {
    let message_id = MessageId::from_external(ExternalId::new("role-alias-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
    let event = ProvEvent::message_sent_global(
        ContextId::new(111, 1),
        message_id,
        "assistant".to_string(),
        vec!["hello".to_string()],
        None,
        agent_id,
        1,
        Vec::new(),
    );

    let normalized = normalize_event(&event).expect("normalize message role alias");
    let role_values: Vec<String> = normalized
        .document
        .entities()
        .filter(|(id, _)| id.as_str().starts_with("message:"))
        .filter_map(|(_, entity)| entity.attributes.get("a2a:role"))
        .filter_map(serde_json::Value::as_str)
        .map(ToString::to_string)
        .collect();

    assert!(
        role_values.iter().any(|role| role == "ROLE_AGENT"),
        "message entity role must be canonical ROLE_AGENT"
    );
}

#[test]
fn normalize_rejects_empty_message_role() {
    let message_id = MessageId::from_external(ExternalId::new("role-empty-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
    let event = ProvEvent::message_received_global(
        ContextId::new(112, 1),
        message_id,
        "".to_string(),
        vec!["hello".to_string()],
        None,
        agent_id,
        1,
    );

    let err = normalize_event(&event).expect_err("empty role must fail normalization");
    let err_text = err.to_string();
    assert!(
        err_text.contains("message role must be non-empty"),
        "expected empty role rejection, got: {err_text}"
    );
}

#[test]
fn normalize_llm_call_rejects_unknown_provider_type() {
    let event = ProvEvent::llm_call_completed_task(
        ContextId::new(120, 1),
        TaskId::from_external(ExternalId::new("task-unknown-provider")),
        "unknown".to_string(),
        "unknown".to_string(),
        "RequirementsPhase".to_string(),
        serde_json::json!({
            "model": "x-ai/grok-4.3",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        serde_json::json!({
            "agent_id": "00000000-0000-0000-0000-000000000001",
            "message_id": "msg-unknown-provider"
        }),
        LlmUsage::Known {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            cached_input_tokens: None,
        },
        20,
        Outcome::Success,
    );

    let err = normalize_event(&event).expect_err("unknown provider type must fail normalization");
    let err_text = err.to_string();
    assert!(
        err_text.contains("a2a:client"),
        "expected missing provider/client field error, got: {err_text}"
    );
}

#[test]
fn normalize_llm_call_backfills_model_from_prompt() {
    let event = ProvEvent::llm_call_completed_task(
        ContextId::new(121, 1),
        TaskId::from_external(ExternalId::new("task-model-backfill")),
        "openrouter".to_string(),
        "unknown".to_string(),
        "RequirementsPhase".to_string(),
        serde_json::json!({
            "model": "x-ai/grok-4.3",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        serde_json::json!({
            "agent_id": "00000000-0000-0000-0000-000000000001",
            "message_id": "msg-model-backfill"
        }),
        LlmUsage::Known {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            cached_input_tokens: None,
        },
        20,
        Outcome::Success,
    );

    let normalized = normalize_event(&event).expect("normalize llm call with prompt model");
    let llm_activity = normalized
        .document
        .activities()
        .find(|(id, _)| id.as_str().starts_with("llm_call:"))
        .expect("llm activity");

    let client = llm_activity
        .1
        .attributes
        .get("a2a:client")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let model = llm_activity
        .1
        .attributes
        .get("a2a:model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    assert_eq!(client, "openrouter");
    assert_eq!(model, "x-ai/grok-4.3");
}

#[test]
fn normalize_agent_stopped_produces_stop_node() {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap());
    let event = ProvEvent::agent_stopped(agent_id, "shutdown".to_string());
    let normalized = normalize_event(&event).expect("normalize agent_stopped");

    let activities: Vec<_> = normalized.document.activities().collect();
    assert_eq!(
        activities.len(),
        1,
        "AgentStopped should produce exactly one activity"
    );

    let (id, activity) = &activities[0];
    assert!(
        id.as_str().starts_with("agent_stop:"),
        "activity id should have agent_stop prefix, got: {}",
        id.as_str()
    );
    assert_eq!(
        activity.prov_type.as_deref(),
        Some(a2a_types::AGENT_STOP),
        "activity prov_type should be a2a:AgentStop"
    );
    assert_eq!(
        activity
            .attributes
            .get("a2a_stop_reason")
            .and_then(serde_json::Value::as_str),
        Some("shutdown"),
        "activity should carry the stop reason"
    );

    assert_eq!(
        normalized.document.was_associated_with().count(),
        1,
        "AgentStop must have a WAS_ASSOCIATED_WITH edge to AgentRuntimeInstance"
    );
}
