//! Phase 2: Resume scope and read-after-write assertion.
//!
//! Invariant: after `insert_message(resume_user_message)` completes,
//! `conversation_context(context_id, limit)` must return items that include
//! the resume message (and prior turn messages when present).

#![recursion_limit = "256"]

mod common;

use std::sync::Arc;

use baml_rt_a2a::{
    a2a_store::{ConversationContextSource, TaskRepository, TaskStore},
    a2a_types::{Message, MessageRole, Part},
};
use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use tokio::sync::Mutex;

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

/// Asserts that conversation_context for context_id contains at least one item
/// whose content includes expected_substring.
fn assert_conversation_contains(
    items: &[baml_rt_provenance::ProvenanceConversationContextItem],
    expected_substring: &str,
) {
    use baml_rt_provenance::store::ConversationItemContent;
    let contents: Vec<String> = items
        .iter()
        .filter_map(|item| match &item.content {
            ConversationItemContent::Message(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(
        contents.iter().any(|s| s.contains(expected_substring)),
        "expected conversation_context to contain text {:?}; got contents: {:?}",
        expected_substring,
        contents
    );
}

/// Resume invariant: after insert_message(resume_message), conversation_context
/// returns the resume message and (when present) prior turn messages.
#[tokio::test]
async fn test_resume_message_visible_after_insert() {
    let store: Arc<Mutex<TaskStore>> = Arc::new(Mutex::new(TaskStore::new()));
    let context_id = ContextId::new(10, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-resume-1"));

    // Ensure task exists (resume path: task was created on first turn)
    let task = common::minimal_task(&task_id, &context_id, None);
    let _ = (*store).upsert(task).await;

    // First turn user message (simulate prior turn)
    let first_msg = make_message(
        "msg-first",
        context_id.clone(),
        Some(task_id.clone()),
        "first turn",
    );
    (*store).insert_message(&first_msg).await.unwrap();

    // Resume user message (what the client sends on turn 2)
    let resume_text = "resume reply";
    let resume_msg = make_message(
        "msg-resume",
        context_id.clone(),
        Some(task_id.clone()),
        resume_text,
    );
    (*store).insert_message(&resume_msg).await.unwrap();

    // Read-after-write: conversation_context must see both messages
    let items = (*store)
        .conversation_context(&context_id, Some(40))
        .await
        .expect("conversation_context");

    assert!(
        !items.is_empty(),
        "conversation_context must return at least one item after insert_message"
    );
    assert_conversation_contains(&items, resume_text);
    assert_conversation_contains(&items, "first turn");
}
