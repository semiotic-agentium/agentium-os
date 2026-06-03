// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Write/read contract for normalized `user_speaker_kind` on user transcript rows.

use baml_rt_conversation::view::UserSpeakerKind;
use baml_rt_core::ids::{ContextId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{ProvEvent, ProvenanceContextReader, ProvenanceWriter};
use test_support::testing::provenance_fixtures::build_isolated_store;

#[tokio::test]
async fn a2a_child_context_user_message_classified_as_relay() {
    let store = build_isolated_store().await;
    let caller = ContextId::new(1_700_000_000_000, 7);
    let child_task_id = TaskId::for_delegated_child(
        UuidId::parse_str("00000000-0000-0000-0000-0000000000a2").unwrap(),
    );
    let context_id =
        ContextId::for_a2a_child(&caller, "argument-cleese", "default", &child_task_id);
    let agent_id = baml_rt_core::ids::AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
    );

    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            MessageId::from_external(baml_rt_core::ids::ExternalId::new("delegated-msg-1")),
            "user".to_string(),
            vec!["delegate this".to_string()],
            None,
            agent_id,
            1_700_000_000_001,
        ))
        .await
        .expect("message_received");

    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].role, "user");
    assert_eq!(items[0].user_speaker_kind, Some(UserSpeakerKind::Relay));
}
