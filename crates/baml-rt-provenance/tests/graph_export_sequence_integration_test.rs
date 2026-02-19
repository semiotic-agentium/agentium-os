use baml_rt_core::Outcome;
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::graph_export::sequence::render_sequence_diagram;
use baml_rt_provenance::graph_export::simplify::simplify_graph;
use baml_rt_provenance::{
    AgentType, GraphExporter, GraphqliteStoreBuilder, LlmUsage, ProvEvent, ProvenanceWriter,
};
use tempfile::tempdir;

/// End-to-end check for file-backed provenance export:
/// write events -> export graph by context -> simplify -> render Mermaid sequence.
///
/// This guards the GraphQLite path used by the graph_exporter binary so storage/query
/// refactors do not silently break sequence diagrams.
#[tokio::test]
async fn file_backed_export_renders_expected_sequence_flow() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("provenance_sequence.db");
    let store = GraphqliteStoreBuilder::file(&db_path)
        .build()
        .expect("build store");

    let context_id = ContextId::new(1_771_470_000_000, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-sequence-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000077").unwrap());

    store
        .add_event(ProvEvent::agent_booted(
            context_id.clone(),
            agent_id.clone(),
            AgentType::new("clickup_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "clickup@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");
    store
        .add_event(ProvEvent::task_created(
            context_id.clone(),
            task_id.clone(),
            agent_id,
        ))
        .await
        .expect("task_created");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new("msg-user-1")),
            "user".to_string(),
            vec!["how many tasks are in to do?".to_string()],
            None,
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
            1_771_470_000_010,
        ))
        .await
        .expect("message_sent");

    let export_store = GraphqliteStoreBuilder::file(&db_path)
        .build()
        .expect("build export store");
    let exporter = GraphExporter::new(export_store);
    let graph = exporter
        .export_by_context(context_id.as_str())
        .await
        .expect("export_by_context");
    assert!(
        !graph.nodes.is_empty(),
        "expected exported graph to have nodes"
    );

    let simplified = simplify_graph(&graph);
    let mermaid = render_sequence_diagram(&simplified);

    assert!(mermaid.contains("sequenceDiagram"));
    assert!(mermaid.contains("actor User"));
    assert!(mermaid.contains("participant clickup_agent"));
    assert!(mermaid.contains("participant clickup"));
    assert!(mermaid.contains("User->>clickup_agent: how many tasks are in to do?"));
    assert!(mermaid.contains("Note over clickup_agent: LLM openai-generic (7475ms"));
    assert!(mermaid.contains("clickup_agent->>clickup: action=ListTeams"));
    assert!(mermaid.contains("clickup-->>clickup_agent: 976ms"));
    assert!(mermaid.contains("clickup_agent->>User: Found 1 team(s)"));

    let user_pos = mermaid.find("User->>clickup_agent:").expect("user message");
    let llm_pos = mermaid
        .find("Note over clickup_agent: LLM openai-generic")
        .expect("llm note");
    let tool_pos = mermaid
        .find("clickup_agent->>clickup: action=ListTeams")
        .expect("tool call");
    let final_pos = mermaid
        .find("clickup_agent->>User: Found 1 team(s)")
        .expect("agent response");
    assert!(
        user_pos < llm_pos && llm_pos < tool_pos && tool_pos < final_pos,
        "expected user -> llm -> tool -> final response ordering, got:\n{mermaid}"
    );
}
