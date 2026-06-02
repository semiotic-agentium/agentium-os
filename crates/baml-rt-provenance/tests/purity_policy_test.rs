// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Policy tests to keep provenance core typed and graph-native.

const EVENTS_RS: &str = include_str!("../src/events.rs");
const NORMALIZER_RS: &str = include_str!("../src/normalizer.rs");
const STORE_RS: &str = include_str!("../src/store.rs");
const EFFECT_SUBSCRIBER_RS: &str = include_str!("../src/effect_subscriber.rs");
const A2A_TRANSPORT_RS: &str = include_str!("../../baml-rt-a2a/src/a2a_transport.rs");

#[test]
fn provenance_events_do_not_use_value_for_core_metadata() {
    assert!(
        !EVENTS_RS.contains("metadata: Value"),
        "policy violated: core provenance events must not use `metadata: Value`",
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
            "policy violated in events.rs: identity field must use a typed newtype, found `{pattern}`",
        );
        assert!(
            !NORMALIZER_RS.contains(pattern),
            "policy violated in normalizer.rs: identity field must use a typed newtype, found `{pattern}`",
        );
        assert!(
            !STORE_RS.contains(pattern),
            "policy violated in store.rs: identity field must use a typed newtype, found `{pattern}`",
        );
    }
}

#[test]
fn citation_drift_uses_graph_backed_ref_hydration_when_store_wired() {
    assert!(
        EFFECT_SUBSCRIBER_RS.contains("prepare_ref_table_for_projection"),
        "citation drift must hydrate refs from Surreal when provenance_store is set"
    );
    assert!(
        EFFECT_SUBSCRIBER_RS.contains("set_provenance_store"),
        "effect subscriber must accept SurrealProvenanceStore for ref hydration"
    );
}

#[test]
fn live_conversation_projection_uses_graph_backed_ref_prepare() {
    assert!(
        A2A_TRANSPORT_RS.contains("prepare_ref_table_for_projection"),
        "ProjectingConversationContextProvider must DB-sync refs before prompt projection"
    );
}
