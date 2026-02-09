use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use baml_rt_provenance::{A2aRelationType, ProvEvent, normalize_event};

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
