//! Query-shape inventory: seeded high-cardinality context + EXPLAIN smoke for hot read paths.

use std::{sync::Arc, time::Instant};

use baml_rt_core::{
    Outcome,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, LlmUsage, ProvEvent, ProvenanceQueryApi, ProvenanceWriter, SurrealStoreBuilder,
    context_metrics_queries, episode::EpisodeReader, graph_export::GraphExporter,
    task_graph_reader::TaskGraphReader,
};
use serde_json::Value;

async fn isolated_store() -> Arc<baml_rt_provenance::SurrealProvenanceStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store")
}

async fn seed_high_cardinality_context(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    context_id: &ContextId,
    task_id: &TaskId,
    agent_id: &AgentId,
    message_count: usize,
) {
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("perf_agent").expect("type"),
            "1.0.0".to_string(),
            "perf@1.0.0".to_string(),
        ))
        .await
        .expect("boot");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("te");

    for i in 0..message_count {
        let message_id = MessageId::from_external(ExternalId::new(format!("perf-msg-{i}")));
        let msg_external = message_id.as_str().to_string();
        let event_order = 1_900_000_000_000_u64 + u64::try_from(i * 2).expect("event order");
        store
            .add_event(ProvEvent::message_received_task(
                context_id.clone(),
                task_id.clone(),
                message_id,
                "user".to_string(),
                vec![format!("message {i}")],
                None,
                agent_id.clone(),
                event_order,
            ))
            .await
            .expect("msg");
        store
            .add_event(ProvEvent::llm_call_completed_task(
                context_id.clone(),
                task_id.clone(),
                "DefaultClient".to_string(),
                "openai-generic".to_string(),
                "Chat".to_string(),
                serde_json::json!({"messages": [{"role": "user", "content": format!("message {i}")}]}),
                serde_json::json!({
                    "agent_id": agent_id.as_str(),
                    "task_id": task_id.as_str(),
                    "message_id": msg_external,
                }),
                LlmUsage::Known {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    cached_input_tokens: None,
                },
                100,
                Outcome::Success,
            ))
            .await
            .expect("llm");
    }
}

async fn explain_smoke(store: &baml_rt_provenance::SurrealProvenanceStore, sql: &str) -> String {
    let explain_sql = format!("EXPLAIN {sql}");
    let mut response = store
        .db()
        .query(&explain_sql)
        .await
        .unwrap_or_else(|e| panic!("EXPLAIN failed for `{explain_sql}`: {e}"));
    let rows: Vec<Value> = response.take(0).expect("explain rows");
    serde_json::to_string(&rows).unwrap_or_default()
}

async fn table_info(store: &baml_rt_provenance::SurrealProvenanceStore, table: &str) -> String {
    let mut response = store
        .db()
        .query(format!("INFO FOR TABLE {table}"))
        .await
        .expect("info for table");
    let rows: Vec<Value> = response.take(0).expect("info rows");
    serde_json::to_string(&rows).unwrap_or_default()
}

#[tokio::test]
async fn provenance_hot_read_paths_and_explain_inventory() {
    let store = isolated_store().await;
    let context_id = ContextId::new(1_900_000_100_000, 1);
    let task_id = TaskId::from_external(ExternalId::new("perf-task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap());
    seed_high_cardinality_context(&store, &context_id, &task_id, &agent_id, 24).await;

    let ctx = context_id.as_str();

    let t0 = Instant::now();
    let convo = store
        .query_conversation_context(&context_id, None, Some(&task_id), None)
        .await
        .expect("conversation context");
    let convo_ms = t0.elapsed().as_millis();
    assert!(
        convo.len() >= 24,
        "expected seeded messages in conversation context, got {}",
        convo.len()
    );

    let t1 = Instant::now();
    let _episode = EpisodeReader::new(Arc::clone(&store))
        .read_snapshot_by_task_id(&task_id)
        .await
        .expect("episode snapshot");
    let episode_ms = t1.elapsed().as_millis();

    let t2 = Instant::now();
    let exporter = GraphExporter::new(Arc::clone(&store));
    let graph = exporter.export_by_context(ctx).await.expect("graph export");
    let export_ms = t2.elapsed().as_millis();
    assert!(!graph.nodes.is_empty());

    let t3 = Instant::now();
    let session_totals = context_metrics_queries::session_totals_by_context(&store, ctx)
        .await
        .expect("session totals");
    let metrics_ms = t3.elapsed().as_millis();
    assert!(
        !session_totals.is_empty(),
        "expected LLM session totals for seeded context"
    );

    let t4 = Instant::now();
    let _tasks = store.list_scoped(&context_id).await.expect("list tasks");
    let task_list_ms = t4.elapsed().as_millis();

    let ctx_node = baml_rt_provenance::id_semantics::context_entity_id_string(ctx);
    let scoped = baml_rt_provenance::vocabulary::context_scope::SCOPED_TO;

    let scoped_edge_sql = format!(
        "SELECT VALUE from_id FROM prov_edge WHERE to_id = '{ctx_node}' AND rel_type = '{scoped}'"
    );
    let scoped_explain = explain_smoke(&store, &scoped_edge_sql).await;
    assert!(
        scoped_explain.contains("idx_edge_to_rel")
            || scoped_explain.contains("TableScan")
            || scoped_explain.contains("IndexScan"),
        "scoped edge EXPLAIN: {scoped_explain}"
    );

    let context_nodes_sql = format!(
        "SELECT node_id, props.a2a_event_order AS event_order FROM prov_node \
         WHERE node_id IN ({scoped_edge_sql}) ORDER BY event_order ASC"
    );
    let nodes_explain = explain_smoke(&store, &context_nodes_sql).await;
    assert!(
        nodes_explain.contains("TableScan") || nodes_explain.contains("IndexScan"),
        "context node sort EXPLAIN: {nodes_explain}"
    );

    let head_sql = "SELECT from_id, to_id FROM prov_edge WHERE from_id IN ['x'] AND rel_type = 'WAS_LAST_TRANSITIONED_TO'";
    let head_explain = explain_smoke(&store, head_sql).await;
    assert!(
        head_explain.contains("idx_edge_from_rel_to_label")
            || head_explain.contains("idx_edge_rel_from_to_label")
            || head_explain.contains("idx_edge_composite")
            || head_explain.contains("IndexScan"),
        "head-pointer edge EXPLAIN: {head_explain}"
    );

    tracing::info!(
        convo_ms,
        episode_ms,
        export_ms,
        metrics_ms,
        task_list_ms,
        convo_rows = convo.len(),
        graph_nodes = graph.nodes.len(),
        session_metric_rows = session_totals.len(),
        "provenance_query_perf_inventory"
    );
}

#[tokio::test]
async fn schema_defines_performance_indexes() {
    let store = isolated_store().await;
    let edge_info = table_info(&store, "prov_edge").await;
    for name in [
        "idx_edge_from_rel_to_label",
        "idx_edge_rel_from_to_label",
        "idx_edge_to_rel",
    ] {
        assert!(
            edge_info.contains(name),
            "missing prov_edge index {name}: {edge_info}"
        );
    }

    let node_info = table_info(&store, "prov_node").await;
    for name in [
        "idx_node_label_event_order",
        "idx_node_label_prov_time",
        "idx_node_label_activity_anchor",
    ] {
        assert!(
            node_info.contains(name),
            "missing prov_node index {name}: {node_info}"
        );
    }
}
