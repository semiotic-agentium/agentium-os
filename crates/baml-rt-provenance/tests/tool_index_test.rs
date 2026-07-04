// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tool index tests using SurrealDB in-memory store.

use std::sync::Arc;

use baml_rt_provenance::index_tools;
use baml_rt_tools::{
    SecretRequest, ToolCapability, ToolFunctionMetadataExport, ToolName, ToolTypeSpec,
};
use serde_json::{Value, json};
use test_support::testing::provenance_fixtures::build_isolated_store;

fn weather_tool() -> ToolFunctionMetadataExport {
    let name = ToolName::parse("support/get_weather").expect("valid tool name");
    ToolFunctionMetadataExport {
        name,
        class_name: "SupportGetWeather".to_string(),
        description: "Fetch a weather report by location".to_string(),
        open_input_schema: json!({ "type": "object" }),
        input_schema: json!({ "type": "object", "properties": { "location": { "type": "string" } } }),
        output_schema: json!({ "type": "object", "properties": { "temperature": { "type": "number" } } }),
        open_input_type: ToolTypeSpec {
            name: "()".to_string(),
            ts_decl: None,
        },
        input_type: ToolTypeSpec {
            name: "WeatherInput".to_string(),
            ts_decl: None,
        },
        output_type: ToolTypeSpec {
            name: "WeatherOutput".to_string(),
            ts_decl: None,
        },
        baml_decl: None,
        extra_ts_decls: Vec::new(),
        tags: vec!["weather".to_string(), "forecast".to_string()],
        secret_requests: vec![SecretRequest::api_key(
            "WEATHER_KEY",
            "Required to call the weather provider",
            "Weather API key",
        )],
        config: None,
        config_bundle: None,
        access: None,
        origin: baml_rt_tools::ToolOrigin::Host,
        backend: baml_rt_tools::ToolBackend::default(),
        digest: None,
        projection_semantics: None,
        session_policy: baml_rt_tools::SessionPolicy::default(),
        capability: baml_rt_tools::ToolCapability::default(),
        invocation_mode: baml_rt_tools::capability_invocation_mode(ToolCapability::default())
            .to_string(),
        event_sources: Vec::new(),
        coordination_baml: None,
    }
}

#[tokio::test]
async fn tool_index_creates_tool_nodes() {
    let store = build_isolated_store().await;

    let tools = vec![weather_tool()];

    index_tools(&store, &tools).await.expect("index tools");

    let mut result = store
        .db()
        .query("SELECT node_id OMIT id FROM prov_node WHERE label = 'ToolFunction' LIMIT 5")
        .await
        .expect("query all");
    let rows: Vec<Value> = result.take(0).unwrap_or_default();
    assert!(
        !rows.is_empty(),
        "expected at least one ToolFunction node after index_tools"
    );

    let mut by_id = store
        .db()
        .query("SELECT * OMIT id FROM prov_node WHERE label = 'ToolFunction' AND node_id = $id LIMIT 1")
        .bind(("id", "support/get_weather"))
        .await
        .expect("query by id");
    let id_rows: Vec<Value> = by_id.take(0).unwrap_or_default();
    assert_eq!(
        id_rows.len(),
        1,
        "expected one ToolFunction node for support/get_weather"
    );
}

/// Regression for the multi-pod shared-SurrealDB publish race (#546): every
/// runner pod indexes the same tool rows into one database concurrently, so the
/// writers collide on the same `prov_node` record. `index_tools` runs its UPSERT
/// under the store's MVCC retry budget; without it the loser of each concurrent
/// pair surfaces `Transaction conflict` and its tool-metadata index is silently
/// dropped. N parallel `index_tools` calls of the same tool must all return `Ok`
/// and converge on exactly one node. (Verified to fail reliably when the retry
/// loop is reverted.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_tool_index_writes_succeed_under_mvcc_contention() {
    // Reproduce the multi-pod race: many writers hammering the same `prov_node`
    // row concurrently. A single UPSERT is a narrow conflict window, so each
    // writer loops to widen it — on a multi-thread runtime this reliably drives
    // SurrealDB `Transaction conflict` for the retry loop to absorb (verified to
    // fail without the loop). 12 writers > 6 retry-budget headroom.
    const PARALLEL_WRITERS: usize = 12;
    const UPSERTS_PER_WRITER: usize = 50;

    let store = build_isolated_store().await;

    let mut handles = Vec::with_capacity(PARALLEL_WRITERS);
    for _ in 0..PARALLEL_WRITERS {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for _ in 0..UPSERTS_PER_WRITER {
                index_tools(&store, &[weather_tool()]).await?;
            }
            Ok::<(), baml_rt_provenance::ProvenanceError>(())
        }));
    }

    let mut failures: Vec<String> = Vec::new();
    for (i, handle) in handles.into_iter().enumerate() {
        match handle.await.expect("writer task panicked") {
            Ok(()) => {}
            Err(e) => failures.push(format!("writer[{i}]: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "all {PARALLEL_WRITERS} concurrent tool-index writers must succeed under MVCC retry; failures: {failures:?}"
    );

    let mut by_id = store
        .db()
        .query("SELECT * OMIT id FROM prov_node WHERE label = 'ToolFunction' AND node_id = $id")
        .bind(("id", "support/get_weather"))
        .await
        .expect("query by id");
    let id_rows: Vec<Value> = by_id.take(0).unwrap_or_default();
    assert_eq!(
        id_rows.len(),
        1,
        "concurrent UPSERTs keyed on node_id must converge on exactly one ToolFunction node"
    );
}
