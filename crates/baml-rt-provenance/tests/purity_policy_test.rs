//! Policy tests to keep provenance core typed and graph-native.

const EVENTS_RS: &str = include_str!("../src/events.rs");
const NORMALIZER_RS: &str = include_str!("../src/normalizer.rs");
const STORE_RS: &str = include_str!("../src/store.rs");

#[test]
fn provenance_events_do_not_use_value_for_core_metadata() {
    assert!(
        !EVENTS_RS.contains("metadata: Value"),
        "Phase 2 policy violated: core provenance events must not use `metadata: Value`",
    );
}

#[test]
fn provenance_core_identity_fields_are_not_stringly_typed() {
    let forbidden = [
        "event_id: String",
        "message_id: String",
        "task_id: String",
        "context_id: String",
        "agent_id: String",
    ];
    for pattern in forbidden {
        assert!(
            !EVENTS_RS.contains(pattern),
            "Phase 1 policy violated in events.rs: found `{pattern}`",
        );
        assert!(
            !NORMALIZER_RS.contains(pattern),
            "Phase 1 policy violated in normalizer.rs: found `{pattern}`",
        );
        assert!(
            !STORE_RS.contains(pattern),
            "Phase 1 policy violated in store.rs: found `{pattern}`",
        );
    }
}
