//! Phase 2: Resume scope and read-after-write assertion.
//!
//! Invariant: after `insert_message(resume_user_message)` completes, the **provenance graph** must
//! expose both turns via `context_messages` (and thus the mounted Surreal store).
//!
//! Note: `conversation_context` currently skips Message rows without `a2a_event_id` in props; the
//! task-store insert path may still satisfy `context_messages` first — full alignment is a separate
//! graph-projection tightening.

#![recursion_limit = "256"]

use std::sync::Arc;

use baml_rt_a2a::{
    a2a_store::{ProvenanceTaskStore, TaskRepository},
    a2a_types::{Message, MessageRole, Part},
};
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId, UuidId};
use baml_rt_provenance::{ProvenanceContextReader, ProvenanceWriter, SurrealStoreBuilder};
use uuid::Uuid;

fn make_message(
    message_id: &str,
    context_id: ContextId,
    task_id: Option<TaskId>,
    text: &str,
) -> Message {
    Message {
        message_id: baml_rt_a2a::a2a_types::A2aMessageId::incoming(ExternalId::new(message_id)),
        role: MessageRole::User,
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Default::default()
        }],
        context_id: Some(context_id),
        task_id,
        reference_task_ids: vec![],
        extensions: vec![],
        metadata: None,
        extra: Default::default(),
    }
}

fn assert_context_messages_contain(messages: &[baml_rt_provenance::ProvenanceContextMessage], needle: &str) {
    let flat: String = messages
        .iter()
        .flat_map(|m| m.content.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        flat.contains(needle),
        "expected context messages to contain {:?}; got {:?}",
        needle,
        messages
    );
}

/// Resume invariant: after insert_message(resume_message), conversation_context
/// returns the resume message and (when present) prior turn messages.
#[tokio::test]
async fn test_resume_message_visible_after_insert() {
    let prov = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory isolated provenance store");
    let writer: Arc<dyn ProvenanceWriter> = prov.clone();
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let store = Arc::new(ProvenanceTaskStore::new(Some(writer), agent_id));

    let context_id = ContextId::new(10, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-resume-1"));

    // `insert_message` records TaskExists + TaskExecutionStarted + message lifecycle to the graph.
    // (Avoid a separate upsert here: duplicate task-scoped provenance events can confuse ordering.)

    // First turn user message (simulate prior turn)
    let first_msg = make_message(
        "msg-first",
        context_id.clone(),
        Some(task_id.clone()),
        "first turn",
    );
    store
        .insert_message(&first_msg)
        .await
        .expect("insert first");

    // Resume user message (what the client sends on turn 2)
    let resume_text = "resume reply";
    let resume_msg = make_message(
        "msg-resume",
        context_id.clone(),
        Some(task_id.clone()),
        resume_text,
    );
    store
        .insert_message(&resume_msg)
        .await
        .expect("insert resume");

    let ctx_items = prov
        .context_messages(&context_id, Some(40))
        .await
        .expect("context_messages");
    assert!(
        !ctx_items.is_empty(),
        "context_messages must return rows after ProvenanceTaskStore::insert_message"
    );
    assert_context_messages_contain(&ctx_items, resume_text);
    assert_context_messages_contain(&ctx_items, "first turn");
}
