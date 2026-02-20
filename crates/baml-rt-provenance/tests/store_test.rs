use baml_rt_core::ids::{ContextId, ExternalId, MessageId};
use baml_rt_provenance::{
    GraphExporter, GraphqliteStoreBuilder, ProvEvent, ProvenanceWriter,
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
    store.add_event(event.clone()).await.expect("persist event");

    let exported = GraphExporter::new(store)
        .export_by_context(event.context_id().as_str())
        .await
        .expect("export graph by context");
    let simplified = simplify_graph(&exported);
    let mermaid = render_sequence_diagram(&simplified);

    assert_snapshot!("normalize_tool_call_started_mermaid", mermaid);
}
