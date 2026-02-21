use baml_rt_core::{
    Outcome,
    ids::{ContextId, ExternalId, TaskId},
};
use baml_rt_provenance::{A2aRelationType, ProvEvent, normalize_event, vocabulary::a2a_roles};

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
