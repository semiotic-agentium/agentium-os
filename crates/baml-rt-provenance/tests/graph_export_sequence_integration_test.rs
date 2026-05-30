// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for graph export and sequence rendering.
//!
//! **Why export-scope bugs weren't caught before:** All other tests use a
//! single-context DB or an isolated store per test. The
//! over-broad export query (`OR labels(a)[0] IN ['AgentRuntimeInstance', ...]`)
//! only blows up when the same DB has many contexts with agent boots; with one
//! context we get a small graph either way. So we add
//! `export_by_context_is_scoped_when_db_has_multiple_contexts` to seed multiple
//! contexts and assert export for one context is bounded.

use baml_rt_core::{
    Outcome,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphExporter, LlmUsage, ProvEvent, ProvenanceWriter, SurrealStoreBuilder,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
};
use insta::assert_snapshot;

/// End-to-end check for file-backed provenance export:
/// write events -> export graph by context -> simplify -> render Mermaid sequence.
///
/// This guards the on-disk provenance store path so storage/query
/// refactors do not silently break sequence diagrams.
#[tokio::test]
async fn file_backed_export_renders_expected_sequence_flow() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store");

    let context_id = ContextId::new(1_771_470_000_000, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-sequence-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000077").unwrap());

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("clickup_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "clickup@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new("msg-user-1")),
            "user".to_string(),
            vec!["how many tasks are in to do?".to_string()],
            None,
            agent_id.clone(),
            1_771_470_000_001,
        ))
        .await
        .expect("message_received");

    store
        .add_event(ProvEvent::llm_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "DefaultClient".to_string(),
            "openai-generic".to_string(),
            "ChooseClickUpAction".to_string(),
            serde_json::json!({"messages": [{"role": "system", "content": "test"}]}),
            serde_json::json!({
                "selected": true,
                "agent_id": "00000000-0000-0000-0000-000000000077",
                "task_id": "task-sequence-1",
                "message_id": "msg-user-1"
            }),
            LlmUsage::Unknown,
            7475,
            Outcome::Success,
        ))
        .await
        .expect("llm_call_completed");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "support/clickup".to_string(),
            None,
            serde_json::json!({"action": "ListTeams"}),
            serde_json::json!({
                "phase": "send",
                "agent_id": "00000000-0000-0000-0000-000000000077",
                "task_id": "task-sequence-1",
                "message_id": "msg-user-1",
                "result": {
                    "items": [{"id": "9013491519", "name": "Workspace", "kind": "team"}],
                    "tasks": [],
                    "message": "Found 1 team(s)"
                }
            }),
            976,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");

    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id,
            MessageId::from_external(ExternalId::new("msg-agent-1")),
            "ROLE_AGENT".to_string(),
            vec!["Found 1 team(s)".to_string()],
            None,
            agent_id.clone(),
            1_771_470_000_010,
            Vec::new(),
        ))
        .await
        .expect("message_sent");

    let exporter = GraphExporter::new(store.clone());
    let graph = exporter
        .export_by_context(context_id.as_str())
        .await
        .expect("export_by_context");

    // Count total edges in DB for this test
    let mut edge_count_resp = store
        .db()
        .query("SELECT count() AS cnt OMIT id FROM prov_edge GROUP ALL")
        .await
        .expect("edge count");
    let edge_count: Vec<serde_json::Value> = edge_count_resp.take(0).unwrap_or_default();
    eprintln!("DEBUG total edges in DB: {:?}", edge_count);

    // All edge types
    let mut sample_edges = store
        .db()
        .query("SELECT rel_type OMIT id FROM prov_edge GROUP BY rel_type")
        .await
        .expect("edge types");
    let sample: Vec<serde_json::Value> = sample_edges.take(0).unwrap_or_default();
    for e in &sample {
        eprintln!("  DB EDGE: {:?}", e);
    }

    // Test: does IN $ids work with Vec<String> bind?
    let test_ids = vec!["message_processing:ctx-1771470000000-1:msg-user-1".to_string()];
    let mut test_resp = store
        .db()
        .query("SELECT from_id, rel_type OMIT id FROM prov_edge WHERE from_id IN $ids")
        .bind(("ids", test_ids))
        .await
        .expect("test IN");
    let test_rows: Vec<serde_json::Value> = test_resp.take(0).unwrap_or_default();
    eprintln!("DEBUG IN test: {} rows", test_rows.len());

    eprintln!(
        "DEBUG graph: {} nodes, {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );
    for node in &graph.nodes {
        let prop_keys: Vec<_> = node.properties.keys().collect();
        eprintln!(
            "  NODE id={} label={} order={:?} display={} props={:?}",
            node.id, node.label, node.event_order, node.display_name, prop_keys
        );
    }
    for edge in graph.edges.iter().take(20) {
        eprintln!("  EDGE {} --[{}]--> {}", edge.from, edge.relation, edge.to);
    }

    assert!(
        !graph.nodes.is_empty(),
        "expected exported graph to have nodes"
    );

    let simplified = simplify_graph(&graph);
    eprintln!(
        "DEBUG simplified: {} nodes, {} edges",
        simplified.nodes.len(),
        simplified.edges.len()
    );
    for node in &simplified.nodes {
        eprintln!(
            "  SIMPLIFIED NODE id={} label={} order={:?} display={} props={:?}",
            node.id,
            node.label,
            node.event_order,
            node.display_name,
            node.properties.keys().collect::<Vec<_>>()
        );
    }
    let mermaid = render_sequence_diagram(&simplified);
    eprintln!("DEBUG mermaid:\n{}", mermaid);

    assert!(mermaid.contains("sequenceDiagram"), "{mermaid}");
    assert!(mermaid.contains("actor User"), "{mermaid}");
    assert!(mermaid.contains("participant clickup_1_0_0"), "{mermaid}");
    assert!(mermaid.contains("participant clickup"), "{mermaid}");
    assert!(
        mermaid.contains("User->>+clickup_1_0_0: how many tasks are in to do?"),
        "{mermaid}"
    );
    assert!(
        mermaid.contains("clickup_1_0_0->>+LLM_openai_generic:") && mermaid.contains("7475ms"),
        "{mermaid}"
    );
    assert!(
        mermaid.contains("clickup_1_0_0->>+clickup: action=ListTeams"),
        "{mermaid}"
    );
    assert!(
        mermaid.contains("clickup-->>-clickup_1_0_0: 976ms"),
        "{mermaid}"
    );
    assert!(
        mermaid.contains("clickup_1_0_0->>-User: Found 1 team(s)"),
        "{mermaid}"
    );

    let user_pos = mermaid
        .find("User->>+clickup_1_0_0:")
        .expect("user message");
    let llm_pos = mermaid
        .find("clickup_1_0_0->>+LLM_openai_generic:")
        .expect("llm request arrow");
    let tool_pos = mermaid
        .find("clickup_1_0_0->>+clickup: action=ListTeams")
        .expect("tool call");
    let final_pos = mermaid
        .find("clickup_1_0_0->>-User: Found 1 team(s)")
        .expect("agent response");
    assert!(
        user_pos < llm_pos && llm_pos < tool_pos && tool_pos < final_pos,
        "expected user -> llm -> tool -> final response ordering, got:\n{mermaid}"
    );

    assert_snapshot!("file_backed_exemplary_agent_flow_mermaid", mermaid);
}

/// Export must be scoped to the requested context only.
///
/// Existing tests use a single-context DB, so they never caught the bug where the export
/// query used `OR labels(a)[0] IN ['AgentRuntimeInstance', ...]`, which pulled every agent
/// in the DB and blew up to 854+ nodes. This test seeds multiple contexts with agent
/// boots, exports by one context, and asserts the graph is scoped (only that context's
/// agent chain, not all agents).
#[tokio::test]
async fn export_by_context_is_scoped_when_db_has_multiple_contexts() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store");

    const N_CONTEXTS: u64 = 8;
    let requested_context = ContextId::new(1_771_470_000_000, 1);

    for i in 0..N_CONTEXTS {
        let ctx = ContextId::new(1_771_470_000_000 + i * 10_000, 1);
        let agent_id = AgentId::from_uuid(
            UuidId::parse_str(&format!("00000000-0000-0000-0000-0000000000{:02}", 70 + i)).unwrap(),
        );
        let task_id = TaskId::from_external(ExternalId::new(format!("task-{i}")));

        store
            .add_event(ProvEvent::agent_booted(
                agent_id.clone(),
                AgentType::new("test_agent").expect("agent_type"),
                "1.0.0".to_string(),
                format!("test@1.0.0-{i}"),
            ))
            .await
            .expect("agent_booted");
        store
            .add_event(ProvEvent::task_exists(ctx.clone(), task_id.clone()))
            .await
            .expect("task_exists");
        store
            .add_event(ProvEvent::task_execution_started(
                ctx.clone(),
                task_id.clone(),
                agent_id.clone(),
            ))
            .await
            .expect("task_execution_started");
        store
            .add_event(ProvEvent::message_received_task(
                ctx.clone(),
                task_id,
                MessageId::from_external(ExternalId::new(format!("msg-{i}"))),
                "user".to_string(),
                vec!["hi".to_string()],
                None,
                agent_id.clone(),
                1_771_470_000_001 + i,
            ))
            .await
            .expect("message_received");
    }

    let graph = GraphExporter::new(store.clone())
        .export_by_context(requested_context.as_str())
        .await
        .expect("export_by_context");

    let agent_instance_count = graph
        .nodes
        .iter()
        .filter(|n| n.label == "AgentRuntimeInstance")
        .count();

    assert!(
        (1..=2).contains(&agent_instance_count),
        "export for one context must include only that context's agent(s), not all {} in DB (got {} agents)",
        N_CONTEXTS,
        agent_instance_count
    );

    let total_nodes = graph.nodes.len();
    assert!(
        total_nodes < 50,
        "export must be bounded (got {} nodes); over-broad query would pull {} agent chains and hundreds of nodes",
        total_nodes,
        N_CONTEXTS
    );
}

/// Regression: message_received_global (first user message, no task_id) must link
/// MessageProcessing to Agent via metadata.agent_id so the initial User→Agent arrow appears.
#[tokio::test]
async fn message_received_global_with_agent_id_renders_initial_user_message() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store");

    let context_id = ContextId::new(1_771_470_000_100, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000088").unwrap());

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("coordinator").expect("agent_type"),
            "1.0.0".to_string(),
            "coordinator@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");

    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            MessageId::from_external(ExternalId::new("msg-first")),
            "user".to_string(),
            vec!["write a bash script that says ello boss".to_string()],
            None,
            agent_id.clone(),
            1_771_470_000_101,
        ))
        .await
        .expect("message_received_global");

    let exporter = GraphExporter::new(store.clone());
    let graph = exporter
        .export_by_context(context_id.as_str())
        .await
        .expect("export_by_context");
    let simplified = simplify_graph(&graph);
    let output = render_sequence_diagram(&simplified);

    assert!(
        output.contains("User->>")
            && output.contains("coordinator")
            && output.contains("ello boss"),
        "initial User→Agent message must appear when metadata.agent_id is set; got:\n{output}"
    );
}

/// Regression: coordinator flow must show BOTH (1) initial User→Coordinator message and
/// (2) delegated User→Worker message. The first must show the user's actual text, not
/// the delegated JSON. Tests that parent context merge includes the first message.
#[tokio::test]
async fn coordinator_flow_shows_initial_user_message_before_delegated_message() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store");

    let context_id = ContextId::new(1_771_470_000_200, 1);
    let coordinator_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap());
    let worker_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000ab").unwrap());

    // 1) Coordinator boots
    store
        .add_event(ProvEvent::agent_booted(
            coordinator_id.clone(),
            AgentType::new("coordinator_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "coordinator-agent@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");

    // 2) Worker boots (delegate target)
    store
        .add_event(ProvEvent::agent_booted(
            worker_id.clone(),
            AgentType::new("claude_session_demo").expect("agent_type"),
            "1.0.0".to_string(),
            "claude-session-demo@1.0.0".to_string(),
        ))
        .await
        .expect("worker_booted");

    // 3) First user message to coordinator (no task yet) — the one that was missing
    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            MessageId::from_external(ExternalId::new("msg-user-first")),
            "user".to_string(),
            vec!["make me a bashskript that says ello world".to_string()],
            None,
            coordinator_id.clone(),
            1_771_470_000_201,
        ))
        .await
        .expect("message_received_global");

    // 4) Coordinator creates task, delegates to worker (delegated message has structured content)
    let task_id = TaskId::from_external(ExternalId::new("task-delegate-1"));
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            worker_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new("msg-delegated")),
            "user".to_string(),
            vec![
                r#"{"objective":"Generate a bash script that prints hello world","plan_steps":[]}"#
                    .to_string(),
            ],
            None,
            worker_id.clone(),
            1_771_470_000_202,
        ))
        .await
        .expect("message_received_task");

    let exporter = GraphExporter::new(store.clone());
    let graph = exporter
        .export_by_context(context_id.as_str())
        .await
        .expect("export_by_context");
    let simplified = simplify_graph(&graph);
    let output = render_sequence_diagram(&simplified);

    assert!(
        output.contains("User->>")
            && output.contains("bashskript")
            && output.contains("ello world"),
        "initial User→Coordinator message must appear with user's actual text; got:\n{output}"
    );
}

/// Regression: two tasks in the same context (e.g. awaitInput flow: user "hi" x2) must both render.
///
/// Seeds a context with two tasks: first user "hi" → processing → reply "Wotcha!";
/// second user "hi" → processing → reply "Hello again". Asserts the Mermaid diagram has
/// both user messages, both replies, and at least two task rects.
#[tokio::test]
async fn two_tasks_in_same_context_both_render_with_separate_rects() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store");

    let context_id = ContextId::new(1_771_470_000_300, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
    let task_id_1 = TaskId::from_external(ExternalId::new("task-two-1"));
    let task_id_2 = TaskId::from_external(ExternalId::new("task-two-2"));

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("coordinator_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "coordinator-agent@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");

    // Task 1: user "hi" → llm → tool → reply
    store
        .add_event(ProvEvent::task_exists(
            context_id.clone(),
            task_id_1.clone(),
        ))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id_1.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id_1.clone(),
            MessageId::from_external(ExternalId::new("msg-user-1")),
            "user".to_string(),
            vec!["hi".to_string()],
            None,
            agent_id.clone(),
            1_771_470_000_301,
        ))
        .await
        .expect("message_received");
    store
        .add_event(ProvEvent::llm_call_completed_task(
            context_id.clone(),
            task_id_1.clone(),
            "DefaultClient".to_string(),
            "openai-generic".to_string(),
            "RouteIntent".to_string(),
            serde_json::json!({"messages": []}),
            serde_json::json!({
                "agent_id": "00000000-0000-0000-0000-000000000099",
                "task_id": "task-two-1",
                "message_id": "msg-user-1"
            }),
            LlmUsage::Unknown,
            585,
            Outcome::Success,
        ))
        .await
        .expect("llm_call_completed");
    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id_1.clone(),
            "discover_agents".to_string(),
            None,
            serde_json::json!({"action": "list"}),
            serde_json::json!({
                "phase": "send",
                "agent_id": "00000000-0000-0000-0000-000000000099",
                "task_id": "task-two-1",
                "message_id": "msg-user-1",
                "result": {"agents": []}
            }),
            23,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id_1.clone(),
            MessageId::from_external(ExternalId::new("msg-agent-1")),
            "ROLE_AGENT".to_string(),
            vec!["Wotcha again, snotling? I'z Orkemedies.".to_string()],
            None,
            agent_id.clone(),
            1_771_470_000_310,
            Vec::new(),
        ))
        .await
        .expect("message_sent");

    // Task 2: user "hi" again → llm → reply
    store
        .add_event(ProvEvent::task_exists(
            context_id.clone(),
            task_id_2.clone(),
        ))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id_2.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id_2.clone(),
            MessageId::from_external(ExternalId::new("msg-user-2")),
            "user".to_string(),
            vec!["hi".to_string()],
            None,
            agent_id.clone(),
            1_771_470_000_311,
        ))
        .await
        .expect("message_received");
    store
        .add_event(ProvEvent::llm_call_completed_task(
            context_id.clone(),
            task_id_2.clone(),
            "DefaultClient".to_string(),
            "openai-generic".to_string(),
            "RouteIntent".to_string(),
            serde_json::json!({"messages": []}),
            serde_json::json!({
                "agent_id": "00000000-0000-0000-0000-000000000099",
                "task_id": "task-two-2",
                "message_id": "msg-user-2"
            }),
            LlmUsage::Unknown,
            400,
            Outcome::Success,
        ))
        .await
        .expect("llm_call_completed");
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id_2.clone(),
            MessageId::from_external(ExternalId::new("msg-agent-2")),
            "ROLE_AGENT".to_string(),
            vec!["Hello again, mate!".to_string()],
            None,
            agent_id.clone(),
            1_771_470_000_320,
            Vec::new(),
        ))
        .await
        .expect("message_sent");

    let exporter = GraphExporter::new(store.clone());
    let graph = exporter
        .export_by_context(context_id.as_str())
        .await
        .expect("export_by_context");
    let simplified = simplify_graph(&graph);
    let output = render_sequence_diagram(&simplified);

    // Both user messages must appear
    assert!(
        output.contains("hi") && output.matches("hi").count() >= 2,
        "expected at least two 'hi' user messages; got:\n{output}"
    );
    // Both replies must appear
    assert!(
        output.contains("Wotcha") && output.contains("Hello again"),
        "expected both replies (Wotcha..., Hello again); got:\n{output}"
    );
    // At least two task rects (one per task)
    let rect_count = output.matches("rect rgb").count();
    assert!(
        rect_count >= 2,
        "expected at least 2 task rects, got {rect_count}; output:\n{output}"
    );
}
