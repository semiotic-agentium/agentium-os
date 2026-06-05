// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External-tool registry store round-trip: import, version assignment, and
//! approved-only `get_latest` semantics.

use std::path::Path;

use baml_rt_repository::{storage::ExternalToolRegistryStore, surreal_store::SurrealStore};
use baml_rt_tools::{
    approval::ApprovalState,
    external_tools::{
        ExternalToolDescribeSnapshot, ExternalToolManifest, ExternalToolSnapshot, InvocationMode,
        METHOD_SCHEMA, MetadataSchemas, ToolSchemaResult, compute_external_schema_digest,
    },
    tools::ToolAccess,
};
use serde_json::json;

fn snapshot(name: &str, description: &str, state: ApprovalState) -> ExternalToolSnapshot {
    let (bundle, local_name) = name.split_once('/').unwrap();
    let manifest = ExternalToolManifest {
        tool_abi_version: "1".to_string(),
        name: name.to_string(),
        description: description.to_string(),
        bundle: bundle.to_string(),
        local_name: local_name.to_string(),
        access_level: ToolAccess::Read,
        tags: vec![],
        invocation_mode: InvocationMode::SingleShot,
        session_policy: Default::default(),
        secrets: vec!["API_KEY".to_string()],
        secret_scope: Default::default(),
        capabilities: json!({}),
        config_bundle: None,
        runtime: None,
        coordination: None,
    };
    let input = json!({"type": "object", "properties": {"q": {"type": "string"}}});
    let output = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}});
    let metadata = manifest.clone().into_metadata(MetadataSchemas {
        input: input.clone(),
        output: output.clone(),
    });
    let schema = ToolSchemaResult {
        schema_version: 1,
        tool_name: name.to_string(),
        content_type: "application/schema+json".to_string(),
        content_digest: compute_external_schema_digest(&metadata).to_string(),
        input,
        output,
    };
    let describe = ExternalToolDescribeSnapshot {
        protocol_version: "1".to_string(),
        supported_methods: vec![METHOD_SCHEMA.to_string()],
        max_payload_bytes: None,
        schema_digest: None,
    };
    let mut snapshot = ExternalToolSnapshot::from_parts(
        Path::new(""),
        manifest,
        schema,
        describe,
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    snapshot.approval.state = state;
    snapshot
}

#[tokio::test]
async fn active_snapshot_is_latest_approved_non_stale() {
    let store = SurrealStore::open_in_memory().await.unwrap();
    let tool = "support/weather";

    // v1 approved → active.
    let v1_snap = snapshot(tool, "build v1", ApprovalState::Approved);
    let v1 = store.put_external_tool_snapshot(&v1_snap).await.unwrap();
    assert_eq!(v1.version, 1);
    assert_eq!(
        store
            .get_latest_external_tool_snapshot(tool)
            .await
            .unwrap()
            .unwrap()
            .snapshot_digest,
        v1_snap.snapshot_digest
    );

    // v2 approved → active moves to v2.
    let v2_snap = snapshot(tool, "build v2", ApprovalState::Approved);
    let v2 = store.put_external_tool_snapshot(&v2_snap).await.unwrap();
    assert_eq!(v2.version, 2);
    assert_eq!(
        store
            .get_latest_external_tool_snapshot(tool)
            .await
            .unwrap()
            .unwrap()
            .snapshot_digest,
        v2_snap.snapshot_digest
    );

    // A pending refresh (v3) must NOT deactivate the approved v2.
    let v3_snap = snapshot(tool, "drifted v3", ApprovalState::Pending);
    let v3 = store.put_external_tool_snapshot(&v3_snap).await.unwrap();
    assert_eq!(v3.version, 3);
    assert_eq!(
        store
            .get_latest_external_tool_snapshot(tool)
            .await
            .unwrap()
            .unwrap()
            .snapshot_digest,
        v2_snap.snapshot_digest
    );

    // Marking the active v2 stale falls back to the previous approved v1.
    store
        .mark_external_tool_version_stale(tool, 2)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_latest_external_tool_snapshot(tool)
            .await
            .unwrap()
            .unwrap()
            .snapshot_digest,
        v1_snap.snapshot_digest
    );

    // Builder source mirrors active selection: one approved snapshot (v1).
    let approved = store.list_approved_external_tool_snapshots().await.unwrap();
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].snapshot_digest, v1_snap.snapshot_digest);

    // Direct version read overlays authoritative row state onto the blob:
    // v2's blob was imported "approved" but the row is now stale.
    let v2_read = store
        .get_external_tool_snapshot(tool, 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v2_read.approval.state, ApprovalState::Stale);
}
