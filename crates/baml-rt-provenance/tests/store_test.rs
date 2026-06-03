// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use baml_rt_core::{
    Outcome,
    ids::{AgentId, ArtifactId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphExporter, LlmUsage, ProvEvent, ProvenanceWriter,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
    metamodel::TaskStatusKind,
    normalize_event,
};
use insta::assert_snapshot;
use serde_json::json;
use test_support::testing::provenance_fixtures::build_isolated_store as shared_isolated_store;

async fn build_isolated_store(_test_name: &str) -> Arc<baml_rt_provenance::SurrealProvenanceStore> {
    let _ = _test_name;
    shared_isolated_store().await
}

fn event_anchor(event: &ProvEvent) -> baml_rt_core::ids::ActivityAnchorId {
    match event {
        ProvEvent::Task(task) => task.id.clone(),
        other => panic!("expected task-scoped event, got {other:?}"),
    }
}

#[tokio::test]
async fn test_normalize_event_snapshot_for_tool_call_started() {
    let event = ProvEvent::tool_call_started_global(
        ContextId::new(1, 1),
        MessageId::from_external(ExternalId::new("msg-1")),
        "tool".to_string(),
        None,
        json!({"input": "value"}),
        json!({"message_id": "msg-1", "agent_id": "00000000-0000-0000-0000-000000000010"}),
        None,
    );

    assert_eq!(event.context_id(), &ContextId::new(1, 1));

    let normalized = normalize_event(&event).expect("normalize event");

    // Snapshot the structural summary: counts + relation roles/types.
    // This catches schema renames without requiring ProvDocument to implement Serialize.
    let mut used_roles: Vec<String> = normalized
        .document
        .used()
        .filter_map(|(_, used)| used.role.clone())
        .collect();
    used_roles.sort();
    let summary = serde_json::json!({
        "entity_count":   normalized.document.entities().count(),
        "activity_count": normalized.document.activities().count(),
        "used_count":     normalized.document.used().count(),
        "used_roles_sorted": used_roles,
        "derived_relation_count": normalized.derived_relations.len(),
    });
    insta::assert_json_snapshot!("tool_call_started_normalized_summary", summary);

    let store = build_isolated_store("normalize-event").await;
    store.add_event(event).await.expect("persist event");
}

#[tokio::test]
async fn test_snapshot_exemplary_mermaid_agent_flow() {
    let store = build_isolated_store("exemplary-agent-flow").await;

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
    let store = build_isolated_store("multiturn-lifecycle").await;

    let context_id = ContextId::new(1_771_470_111_000, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-lifecycle-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000078").unwrap());

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
            vec!["Draft a weekly status update".to_string()],
            None,
            agent_id.clone(),
            1_771_470_111_001,
        ))
        .await
        .expect("message_received_1");
    let submitted = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        None,
        None,
        Some(TaskStatusKind::Submitted),
    );
    let submitted_anchor = event_anchor(&submitted);
    store.add_event(submitted).await.expect("status_submitted");

    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new("msg-agent-1")),
            "ROLE_AGENT".to_string(),
            vec!["Need project scope before I can proceed.".to_string()],
            None,
            agent_id.clone(),
            1_771_470_111_003,
            Vec::new(),
        ))
        .await
        .expect("message_sent_1");
    let input_required_prompt = "Need project scope before I can proceed.".to_string();
    let input_required = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        Some(TaskStatusKind::Submitted),
        Some(submitted_anchor),
        Some(TaskStatusKind::InputRequired {
            prompt: input_required_prompt.clone(),
        }),
    );
    let input_required_anchor = event_anchor(&input_required);
    store
        .add_event(input_required)
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
            agent_id.clone(),
            1_771_470_111_005,
        ))
        .await
        .expect("message_received_2");
    store
        .add_event(ProvEvent::task_status_changed_typed(
            context_id.clone(),
            task_id.clone(),
            Some(TaskStatusKind::InputRequired {
                prompt: input_required_prompt,
            }),
            Some(input_required_anchor),
            Some(TaskStatusKind::Working),
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
            None,
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
            agent_id.clone(),
            1_771_470_111_010,
            Vec::new(),
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

    let exported = GraphExporter::new(store.clone())
        .export_by_context(context_id.as_str())
        .await
        .expect("export graph by context");

    let simplified = simplify_graph(&exported);
    let mermaid = render_sequence_diagram(&simplified);

    assert!(
        mermaid.contains("Draft a weekly status update")
            && mermaid.contains("Use the platform project context."),
        "expected both user turns in mermaid: {mermaid}"
    );
    // TaskState notes removed from sequence diagram (LLM/tool arrows are more useful).
    assert!(
        mermaid.contains("Artifact application/markdown"),
        "expected artifact note in mermaid: {mermaid}"
    );

    assert_snapshot!("exemplary_multiturn_lifecycle_mermaid", mermaid);
}

#[tokio::test]
async fn task_scoped_messages_without_agent_metadata_still_render_sequence_activity() {
    let store = build_isolated_store("task-scoped-message-agent-link").await;

    let context_id = ContextId::new(1_771_470_222_000, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-message-link-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000079").unwrap());

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
            MessageId::from_external(ExternalId::new("msg-user-no-meta")),
            "user".to_string(),
            vec!["hello strict provenance".to_string()],
            None,
            agent_id.clone(),
            1_771_470_222_001,
        ))
        .await
        .expect("message_received");
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id,
            MessageId::from_external(ExternalId::new("msg-agent-no-meta")),
            "ROLE_AGENT".to_string(),
            vec!["acknowledged".to_string()],
            None,
            agent_id.clone(),
            1_771_470_222_002,
            Vec::new(),
        ))
        .await
        .expect("message_sent");

    let exported = GraphExporter::new(store)
        .export_by_context(context_id.as_str())
        .await
        .expect("export graph by context");
    let simplified = simplify_graph(&exported);
    let mermaid = render_sequence_diagram(&simplified);

    assert!(
        mermaid.contains("participant clickup_1_0_0"),
        "expected named agent participant: {mermaid}"
    );
    assert!(
        mermaid.contains("User->>+clickup_1_0_0: hello strict provenance"),
        "expected user message activity line with strict graph semantics: {mermaid}"
    );
    assert!(
        mermaid.contains("clickup_1_0_0->>-User: acknowledged"),
        "expected agent response activity line with strict graph semantics: {mermaid}"
    );
}
