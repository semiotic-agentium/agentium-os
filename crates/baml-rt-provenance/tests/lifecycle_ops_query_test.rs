// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Round-trip test: writing an `AgentStopped` event must make it queryable via
//! [`ProvenanceOpsResource::LifecycleEvents`]. Isolates the writer/reader loop
//! from the HTTP runner so regressions in either side surface in
//! `cargo test -p baml-rt-provenance` alone.
//!
//! Regression coverage for the bug where lifecycle rows were dropped by the
//! outcome-segment retain filter in `SurrealProvenanceStore::query_ops` because
//! `AgentStop` activities have no `a2a_activity_outcome` and were canonicalized
//! to `"Indeterminate"`, which neither `Success` nor `Failed` would accept.
//!
//! ```bash
//! cargo test -p baml-rt-provenance --test lifecycle_ops_query_test
//! ```
use baml_rt_core::ids::{AgentId, UuidId};
use baml_rt_provenance::{
    ProvEvent, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    ProvenanceWriter,
};
use test_support::testing::provenance_fixtures::build_isolated_store;

#[tokio::test]
async fn agent_stopped_is_queryable_as_lifecycle_event() {
    let store = build_isolated_store().await;

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000042").unwrap());

    store
        .add_event(ProvEvent::agent_stopped(agent_id, "undeploy".to_string()))
        .await
        .expect("add_event(agent_stopped) should persist");

    let response = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::LifecycleEvents,
            ..Default::default()
        })
        .await
        .expect("query_ops(LifecycleEvents) should succeed");

    assert_eq!(
        response.rows.len(),
        1,
        "expected one AgentStop row, got: {:?}",
        response.rows
    );
    let stop_reason = response.rows[0]
        .get("a2a_stop_reason")
        .and_then(|v| v.as_str())
        .expect("row should carry a2a_stop_reason");
    assert_eq!(stop_reason, "undeploy");
}
