use baml_rt_core::{
    Outcome,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    A2aRelationType, LlmUsage, ProvEvent, normalize_event, vocabulary::a2a_roles,
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
