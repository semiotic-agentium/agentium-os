// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Cross-reference the metamodel's `expected_edges` and
//! `required_properties` lists against the closed vocabulary set.
//!
//! Edge-label drift (`MAPPING_*::expected_edges` strings that point at
//! edges the normalizer never writes) and property-key drift are caught
//! here at test time. Compile-time enforcement of the same invariant
//! lives in `metamodel::edges::AllowedPrimaryEdge`'s sealed witness
//! traits; this file remains as a backstop for the SurrealQL / serde
//! seam the type system cannot reach.

use std::collections::HashSet;

use baml_rt_provenance::{
    ALL_EVENT_KINDS, EventGraphKind, mapping_for_event_kind,
    vocabulary::{a2a, a2a_relations, prov_relations, semantic_labels},
};

fn known_edge_labels() -> HashSet<&'static str> {
    let mut s = HashSet::new();
    // semantic_labels::* — the canonical PROV-style edge alphabet.
    for label in [
        semantic_labels::WAS_USED_BY,
        semantic_labels::WAS_CLASSIFIED_BY,
        semantic_labels::WAS_CONSUMED_BY,
        semantic_labels::WAS_RECEIVED_BY,
        semantic_labels::WAS_SPAWNED_BY,
        semantic_labels::WAS_UPDATED_BY,
        semantic_labels::WAS_BOOTSTRAPPED_BY,
        semantic_labels::WAS_EMITTED_BY,
        semantic_labels::WAS_GENERATED_BY,
        semantic_labels::WAS_CREATED_BY,
        semantic_labels::WAS_EXECUTED_BY,
        semantic_labels::WAS_INVOKED_BY,
        semantic_labels::WAS_CALLED_BY,
        semantic_labels::WAS_TRANSITIONED_FROM,
        semantic_labels::WAS_TRANSITIONED_TO,
        semantic_labels::WAS_LAST_TRANSITIONED_TO,
        semantic_labels::WAS_LAST_EXECUTED_BY,
        semantic_labels::WAS_INFORMED_BY,
        semantic_labels::WAS_REPLACED_BY,
        semantic_labels::WAS_REFINED_BY,
        semantic_labels::WAS_RELATED_TO,
        semantic_labels::WAS_DELEGATED_TO,
        semantic_labels::WAS_SCHEDULED_FROM,
        semantic_labels::CITED,
        semantic_labels::HAS_INTENT,
        semantic_labels::HAS_PLAN,
    ] {
        s.insert(label);
    }
    // a2a_relations::* — derived A2A_* edges emitted by the dynamic write arm.
    for label in [
        a2a_relations::PLAN_STEP,
        a2a_relations::TASK_MESSAGE,
        a2a_relations::TASK_SESSION_STEP,
        a2a_relations::TASK_ARTIFACT,
        a2a_relations::TASK_CALL,
        a2a_relations::TASK_STATUS_TRANSITION,
        a2a_relations::MESSAGE_CALL,
        a2a_relations::HOST_DISPATCH_TARGET,
    ] {
        s.insert(label);
    }
    // prov_relations::* — the small PROV-W3C residue.
    for label in [
        prov_relations::WAS_ASSOCIATED_WITH,
        prov_relations::WAS_DERIVED_FROM,
    ] {
        s.insert(label);
    }
    s
}

fn known_property_keys() -> HashSet<&'static str> {
    let mut s = HashSet::new();
    for key in [
        a2a::MESSAGE_ID,
        a2a::ROLE,
        a2a::CONTENT,
        a2a::DIRECTION,
        a2a::CLIENT,
        a2a::MODEL,
        a2a::FUNCTION_NAME,
        a2a::AGENT_ID,
        a2a::AGENT_TYPE,
        a2a::AGENT_VERSION,
        a2a::INTENT_ID,
        a2a::PLAN_ID,
        a2a::STEP_ID,
        a2a::TASK_ID,
        a2a::CONTEXT_ID,
        a2a::SUMMARY_TEXT,
        a2a::DURATION_MS,
        a2a::ACTIVITY_OUTCOME,
        a2a::REASON,
        a2a::TOOL_NAME,
        a2a::HOST_INGRESS_KIND,
        a2a::HOST_INGRESS_TARGET_PACKAGE,
        a2a::HOST_INGRESS_TARGET_INSTANCE,
        a2a::HOST_INGRESS_SOURCE_KIND,
        a2a::HOST_INGRESS_SOURCE_KEY,
        a2a::HOST_INGRESS_RECORD_COUNT,
        a2a::HOST_INGRESS_ROUTING_KEY,
    ] {
        s.insert(key);
    }
    s
}

#[test]
fn every_expected_edge_label_is_a_known_vocabulary_constant() {
    let known = known_edge_labels();
    for kind in ALL_EVENT_KINDS {
        let mapping = mapping_for_event_kind(kind);
        for &edge in mapping.expected_edges {
            assert!(
                known.contains(edge),
                "metamodel mapping for {kind:?} declares edge label {edge:?} which is not a known \
                 vocabulary constant. `expected_edges` must reference labels the normalizer \
                 actually writes; either add the label to vocabulary or remove it from the mapping."
            );
        }
    }
}

#[test]
fn every_required_property_key_is_a_known_vocabulary_constant() {
    let known = known_property_keys();
    for kind in ALL_EVENT_KINDS {
        let mapping = mapping_for_event_kind(kind);
        for &prop in mapping.required_properties {
            assert!(
                known.contains(prop),
                "metamodel mapping for {kind:?} declares required property {prop:?} which is not \
                 a known vocabulary constant in `vocabulary::a2a`. Either add the key or remove \
                 the requirement from the mapping."
            );
        }
    }
}

#[test]
fn message_mappings_use_a2a_task_message_not_speculative_task_triggered_by_message() {
    // Regression: the canonical task↔message edge is A2A_TASK_MESSAGE
    // (with a `direction` attribute) sourced from the `TaskHasMessage`
    // derived relation in normalizer.rs. The older speculative
    // `TASK_TRIGGERED_BY_MESSAGE` / `TASK_EMITTED_MESSAGE` labels must
    // not reappear in the mappings; the normalizer never emits them.
    let mr = mapping_for_event_kind(EventGraphKind::MessageReceived);
    let ms = mapping_for_event_kind(EventGraphKind::MessageSent);
    assert!(
        mr.expected_edges.contains(&a2a_relations::TASK_MESSAGE),
        "MAPPING_MESSAGE_RECEIVED must reference the actual A2A_TASK_MESSAGE edge"
    );
    assert!(
        ms.expected_edges.contains(&a2a_relations::TASK_MESSAGE),
        "MAPPING_MESSAGE_SENT must reference the actual A2A_TASK_MESSAGE edge"
    );
    assert!(
        !mr.expected_edges.contains(&"TASK_TRIGGERED_BY_MESSAGE"),
        "MAPPING_MESSAGE_RECEIVED must not reference TASK_TRIGGERED_BY_MESSAGE; \
         the normalizer never writes that label"
    );
    assert!(
        !ms.expected_edges.contains(&"TASK_EMITTED_MESSAGE"),
        "MAPPING_MESSAGE_SENT must not reference TASK_EMITTED_MESSAGE; \
         the normalizer never writes that label"
    );
}
