use baml_rt_core::{
    Outcome,
    ids::{AgentId, ArtifactId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphExporter, GraphqliteStoreBuilder, LlmUsage, ProvEvent, ProvenanceWriter,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
    normalize_event,
};
use insta::assert_snapshot;
use serde_json::json;

#[tokio::test]
async fn test_normalize_event_snapshot_for_tool_call_started() {
    let event = ProvEvent::tool_call_started_global(
        ContextId::new(1, 1),
        MessageId::from_external(ExternalId::new("msg-1")),
        "tool".to_string(),
        None,
        json!({"input": "value"}),
        json!({"message_id": "msg-1", "agent_id": "00000000-0000-0000-0000-000000000010"}),
    );

    assert_eq!(event.context_id(), &ContextId::new(1, 1));

    let normalized = normalize_event(&event).expect("normalize event");
    let has_args_used = normalized
        .document
        .used()
        .any(|(_, used)| used.role.as_deref() == Some("a2a:args"));
    assert!(
        has_args_used,
        "normalized tool call must include USED relation with role a2a:args"
    );

    let store = GraphqliteStoreBuilder::in_memory()
        .build()
        .expect("build store");
    store.add_event(event).await.expect("persist event");
}

#[tokio::test]
async fn test_snapshot_exemplary_mermaid_agent_flow() {
    let store = GraphqliteStoreBuilder::in_memory()
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

    let exported = GraphExporter::new(store)
        .export_by_context(context_id.as_str())
        .await
        .expect("export graph by context");
    let simplified = simplify_graph(&exported);
    let mermaid = render_sequence_diagram(&simplified);

    assert_snapshot!("exemplary_agent_flow_mermaid", mermaid);
}

#[tokio::test]
async fn test_snapshot_exemplary_multiturn_lifecycle_mermaid() {
    let store = GraphqliteStoreBuilder::in_memory()
        .build()
        .expect("build store");

    let context_id = ContextId::new(1_771_470_111_000, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-lifecycle-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000078").unwrap());

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
            vec!["Draft a weekly status update".to_string()],
            None,
            1_771_470_111_001,
        ))
        .await
        .expect("message_received_1");
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            None,
            Some("submitted".to_string()),
        ))
        .await
        .expect("status_submitted");
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new("msg-agent-1")),
            "ROLE_AGENT".to_string(),
            vec!["Need project scope before I can proceed.".to_string()],
            None,
            1_771_470_111_003,
        ))
        .await
        .expect("message_sent_1");
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            Some("submitted".to_string()),
            Some("input-required".to_string()),
        ))
        .await
        .expect("status_input_required");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new("msg-user-2")),
            "user".to_string(),
            vec!["Use the platform project context.".to_string()],
            None,
            1_771_470_111_005,
        ))
        .await
        .expect("message_received_2");
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            Some("input-required".to_string()),
            Some("working".to_string()),
        ))
        .await
        .expect("status_working");
    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "support/notion".to_string(),
            None,
            serde_json::json!({"action":"CreatePage","title":"Weekly Status"}),
            serde_json::json!({
                "phase":"send",
                "agent_id":"00000000-0000-0000-0000-000000000078",
                "task_id":"task-lifecycle-1",
                "result":{"items":[{"id":"page-1"}]}
            }),
            145,
            Outcome::Success,
        ))
        .await
        .expect("tool_call_completed");
    store
        .add_event(ProvEvent::task_artifact_generated(
            context_id.clone(),
            task_id.clone(),
            Some(ArtifactId::from_external(ExternalId::new(
                "artifact-weekly-status",
            ))),
            Some("application/markdown".to_string()),
        ))
        .await
        .expect("artifact_generated");
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new("msg-agent-2")),
            "ROLE_AGENT".to_string(),
            vec!["Draft is ready and attached.".to_string()],
            None,
            1_771_470_111_010,
        ))
        .await
        .expect("message_sent_2");
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id,
            Some("working".to_string()),
            Some("completed".to_string()),
        ))
        .await
        .expect("status_completed");

    let exported = GraphExporter::new(store)
        .export_by_context(context_id.as_str())
        .await
        .expect("export graph by context");
    let simplified = simplify_graph(&exported);
    let mermaid = render_sequence_diagram(&simplified);

    assert!(
        mermaid.contains("status submitted")
            && mermaid.contains("input-required")
            && mermaid.contains("status working"),
        "expected status lifecycle notes in mermaid: {mermaid}"
    );
    assert!(
        mermaid.contains("Artifact application/markdown"),
        "expected artifact note in mermaid: {mermaid}"
    );

    assert_snapshot!("exemplary_multiturn_lifecycle_mermaid", mermaid);
}
