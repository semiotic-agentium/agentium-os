use baml_rt_core::{
    Outcome,
    ids::{ContextId, ExternalId, MessageId, TaskId},
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
        serde_json::json!({"phase":"send","result":{"tasks":[]}}),
        15,
        Outcome::Success,
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
fn normalize_task_scoped_call_with_metadata_message_id_attaches_message_context() {
    let event = ProvEvent::llm_call_completed_task(
        ContextId::new(9, 1),
        TaskId::from_external(ExternalId::new("task-msg-link")),
        "default-client".to_string(),
        "model-x".to_string(),
        "ChooseAction".to_string(),
        serde_json::json!({"messages": []}),
        serde_json::json!({"message_id": "cli-msg-1"}),
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

    let event_a = ProvEvent::message_received_global(
        ContextId::new(100, 1),
        message_id.clone(),
        "ROLE_USER".to_string(),
        vec!["hello".to_string()],
        None,
        1,
    );
    let event_b = ProvEvent::message_received_global(
        ContextId::new(101, 1),
        message_id,
        "ROLE_USER".to_string(),
        vec!["hello".to_string()],
        None,
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
