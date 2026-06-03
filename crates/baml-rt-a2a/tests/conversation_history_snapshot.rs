// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! **Spec regression:** projected conversation rows (same pipeline as BAML `conversation_transcript`) via
//! [`baml_rt_a2a::a2a_transport::ProjectingConversationContextProvider`]: store rows →
//! [`baml_rt_conversation::provenance_item_to_projection_item`] →
//! [`baml_rt_tools::prompt_projection::project_prompt_context`] with **default** options and a
//! **stub** [`baml_rt_tools::ToolRegistry`] (registers `system/discover_agents` so
//! `describe_invocation` is non-empty — matches integration tests that wire a real catalog).
//!
//! Surface: **Message** (`#N`), **ToolCall** + **ToolResult** (execute), **SessionStep** (Open,
//! SearchRead, PageRead; **SendDone** in graph only — not projected), **ContextRefTables** + **ArchiveReader** (live grep/cat).
//! Normative: [`docs/assertions/baml-rt-conversation-spec.md`]. Update: `INSTA_UPDATE=1 cargo test -p baml-rt-a2a --test conversation_history_snapshot`

mod discover_stub {
    use std::sync::Arc;

    use async_trait::async_trait;
    use baml_derive::BamlType;
    use baml_rt_core::Result;
    use baml_rt_tools::{
        ToolCapability, ToolHandler, ToolMetadataBuilder, ToolName, ToolOrigin, ToolSession,
        ToolSessionError, ToolStep, TypeBasedMetadataBuilder,
        tool_schema::DescribeAction,
        tools::{ToolFunctionMetadata, ToolSessionContext},
    };
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    pub struct DiscoverOpenIn {}
    impl DescribeAction for DiscoverOpenIn {
        fn describe(&self) -> String {
            "DiscoverOpenIn".to_string()
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    pub struct DiscoverListIn {
        pub query: String,
    }
    impl DescribeAction for DiscoverListIn {
        fn describe(&self) -> String {
            format!("query={}", self.query)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    pub struct DiscoverListOut {
        pub _ok: bool,
    }

    struct NoopSession;

    #[async_trait]
    impl ToolSession for NoopSession {
        async fn send(&mut self, _input: Value) -> std::result::Result<(), ToolSessionError> {
            Ok(())
        }

        async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
            Ok(ToolStep::Done {
                output: Some(serde_json::json!({ "_ok": true })),
            })
        }

        async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
            Ok(())
        }

        async fn abort(
            &mut self,
            _reason: Option<String>,
        ) -> std::result::Result<(), ToolSessionError> {
            Ok(())
        }
    }

    struct Handler {
        metadata: ToolFunctionMetadata,
    }

    #[async_trait]
    impl ToolHandler for Handler {
        fn metadata(&self) -> &ToolFunctionMetadata {
            &self.metadata
        }

        fn capability(&self) -> ToolCapability {
            ToolCapability::Streaming
        }

        fn describe_invocation(&self, content: &Value) -> String {
            if let Some(q) = content.get("query").and_then(|v| v.as_str()) {
                return format!(r#"discover_agents query={q:?}"#);
            }
            format!("{}", self.metadata.name)
        }

        async fn open_session(
            &self,
            _ctx: ToolSessionContext,
            _open_input: Value,
        ) -> Result<Box<dyn ToolSession>> {
            Ok(Box::new(NoopSession))
        }
    }

    /// Minimal registry so `ToolCall` projection matches production (default options, no JSON fallback).
    pub fn registry() -> baml_rt_tools::ToolRegistry {
        let reg = baml_rt_tools::ToolRegistry::new();
        let name = ToolName::parse("system/discover_agents").expect("parse tool name");
        let class_name = ToolFunctionMetadata::derive_class_name(name.bundle(), name.local());
        let metadata =
            TypeBasedMetadataBuilder::<DiscoverOpenIn, DiscoverListIn, DiscoverListOut>::new(
                name,
                class_name,
                "stub: agent listing for conversation_history integration snapshot".to_string(),
            )
            .with_origin(ToolOrigin::Host)
            .build_metadata();
        reg.register_dynamic(metadata.clone(), Arc::new(Handler { metadata }))
            .expect("register stub discover_agents");
        reg
    }
}

use std::{sync::Arc, time::Duration};

use baml_rt_conversation::view::SessionStepOp;
use baml_rt_core::{
    Outcome,
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    CallScope, ProvEvent, ProvenanceContextReader, ProvenanceWriter, SurrealStoreBuilder,
    prepare_ref_table_for_projection,
};
use baml_rt_tools::{
    archive_read::{ShortRef, format_session_read_from_vtable, render_to_lines},
    archive_refs::{ArchiveEntry, ContextRefTables, get_or_create_ref_table},
    prompt_projection::{
        PromptProjectionContent, PromptProjectionItem, SessionStepPayload, SessionStepProjection,
        project_prompt_context,
    },
};

fn make_archive_reader(
    tables: ContextRefTables,
    context_id: String,
) -> impl Fn(&str, Option<&str>, usize, usize) -> Option<String> {
    move |archive_ref_str, grep_str, offset, limit| {
        let ref_table = baml_rt_tools::archive_refs::get_ref_table(&tables, &context_id)?;
        format_session_read_from_vtable(&ref_table, archive_ref_str, grep_str, offset, limit)
    }
}

async fn wall_clock_tick() {
    tokio::time::sleep(Duration::from_millis(12)).await;
}

#[tokio::test]
async fn conversation_history_renders_like_ctx_tags() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build isolated test store");
    let context_id = ContextId::new(1, 1);
    let ctx_key = context_id.as_str().to_string();
    let msg_id = MessageId::from_external(ExternalId::new("msg-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap());
    let tool_send_anchor: ActivityAnchorId = ActivityAnchorId::from("snap-ctx-hist-tool-send");
    let tool_name = "system/discover_agents".to_string();

    let result_payload = serde_json::json!([
        { "name": "crm-agent", "description": "Business reporting agent" },
        { "name": "dev-agent", "description": "Code generation agent" },
    ]);

    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_id.clone(),
            "user".into(),
            vec!["what can you do".into()],
            None,
            agent_id.clone(),
            1_700_000_000_000,
        ))
        .await
        .expect("message_received");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_call_started_global(
            context_id.clone(),
            msg_id.clone(),
            tool_name.clone(),
            None,
            serde_json::json!({ "query": "all" }),
            serde_json::json!({
                "phase": "execute",
                "agent_id": agent_id.as_str(),
                "message_id": "msg-1",
            }),
            None,
        ))
        .await
        .expect("tool_call_started");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_call_completed_global_with_id(
            tool_send_anchor.clone(),
            context_id.clone(),
            msg_id.clone(),
            tool_name.clone(),
            None,
            serde_json::json!({ "query": "all" }),
            serde_json::json!({
                "phase": "execute",
                "agent_id": agent_id.as_str(),
                "message_id": "msg-1",
                "result": result_payload.clone(),
            }),
            50,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");
    wall_clock_tick().await;

    let scope = CallScope::Message {
        message_id: msg_id.clone(),
    };
    let session_id = "session-abc123".to_string();

    let lines_content = render_to_lines(&result_payload);
    let entry = ArchiveEntry::new(
        lines_content,
        tool_name.clone(),
        Some("found 2 agents".into()),
        tool_send_anchor.as_str().to_string(),
        "tool_result".to_string(),
    );
    let short_at1 = store
        .archive_allocate_and_put(
            &context_id,
            &agent_id,
            tool_send_anchor.as_str(),
            entry.clone(),
        )
        .await
        .expect("durable archive body");
    let tables: ContextRefTables = ContextRefTables::new();
    let archive_ref = short_at1.to_string();
    let header = entry.display_header(short_at1);

    store
        .add_event(ProvEvent::tool_session_step(
            context_id.clone(),
            scope.clone(),
            tool_name.clone(),
            session_id.clone(),
            &SessionStepOp::Open,
        ))
        .await
        .expect("session open");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_session_step(
            context_id.clone(),
            scope.clone(),
            tool_name.clone(),
            session_id.clone(),
            &SessionStepOp::SendDone {
                archive_ref: archive_ref.clone(),
                header: header.clone(),
                informed_by: tool_send_anchor.as_str().to_string(),
            },
        ))
        .await
        .expect("session send_done");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_session_step(
            context_id.clone(),
            scope.clone(),
            tool_name.clone(),
            session_id.clone(),
            &SessionStepOp::SearchRead {
                archive_ref: archive_ref.clone(),
                grep: "name description".into(),
                offset: 0,
                limit: 200,
            },
        ))
        .await
        .expect("session search_read");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_session_step(
            context_id.clone(),
            scope,
            tool_name,
            session_id,
            &SessionStepOp::PageRead {
                archive_ref: archive_ref.clone(),
                offset: 0,
                limit: 200,
            },
        ))
        .await
        .expect("session page_read");
    wall_clock_tick().await;

    let raw_items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");

    let projection_items: Vec<_> = raw_items
        .into_iter()
        .filter_map(baml_rt_conversation::provenance_item_to_projection_item)
        .collect();

    let registry = discover_stub::registry();
    let ref_table =
        prepare_ref_table_for_projection(&store, &context_id, &projection_items, &registry)
            .await
            .expect("graph-backed ref table");
    tables.insert(ctx_key.clone(), Arc::clone(&ref_table));
    let reader = make_archive_reader(tables, ctx_key);
    let history = project_prompt_context(projection_items, &registry, &ref_table, Some(&reader));

    insta::assert_json_snapshot!(history);
}

#[tokio::test]
async fn delegated_context_send_done_replay_seeds_later_archive_read() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build isolated test store");
    let caller_context_id = ContextId::new(1_700_000_000_000, 7);
    let child_task_id = TaskId::for_delegated_child(
        UuidId::parse_str("00000000-0000-0000-0000-0000000000a2").unwrap(),
    );
    let context_id = ContextId::for_a2a_child(
        &caller_context_id,
        "argument-cleese",
        "default",
        &child_task_id,
    );
    let ctx_key = context_id.as_str().to_string();
    let msg_id = MessageId::from_external(ExternalId::new("delegated-msg-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000002").unwrap());
    let tool_send_anchor: ActivityAnchorId = ActivityAnchorId::from("delegated-a2a-tool-send");
    let tool_name = "system/internal_a2a".to_string();
    let result_payload = serde_json::json!({
        "chunks": [
            {
                "message": {
                    "role": "ROLE_AGENT",
                    "parts": [
                        { "text": "counterpoint found in delegated archive" }
                    ]
                }
            }
        ],
        "completion": "Done"
    });

    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_id.clone(),
            "user".into(),
            vec!["delegate this".into()],
            None,
            agent_id.clone(),
            1_700_000_000_001,
        ))
        .await
        .expect("message_received");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_call_started_global(
            context_id.clone(),
            msg_id.clone(),
            tool_name.clone(),
            None,
            serde_json::json!({ "op": "Send" }),
            serde_json::json!({
                "phase": "execute",
                "agent_id": agent_id.as_str(),
                "message_id": msg_id.as_str(),
            }),
            None,
        ))
        .await
        .expect("tool_call_started");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_call_completed_global_with_id(
            tool_send_anchor.clone(),
            context_id.clone(),
            msg_id.clone(),
            tool_name.clone(),
            None,
            serde_json::json!({ "op": "Send" }),
            serde_json::json!({
                "phase": "execute",
                "agent_id": agent_id.as_str(),
                "message_id": msg_id.as_str(),
                "result": result_payload,
            }),
            50,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::tool_session_step(
            context_id.clone(),
            CallScope::Message {
                message_id: msg_id.clone(),
            },
            tool_name.clone(),
            "delegated-session".to_string(),
            &SessionStepOp::SendDone {
                archive_ref: "@8".to_string(),
                header: "@8 · \"delegated result\" · 1L · 64B".to_string(),
                informed_by: tool_send_anchor.as_str().to_string(),
            },
        ))
        .await
        .expect("session send_done");
    wall_clock_tick().await;

    let raw_items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    let seed_items: Vec<_> = raw_items
        .into_iter()
        .filter_map(baml_rt_conversation::provenance_item_to_projection_item)
        .filter(|item| {
            matches!(
                item.content,
                PromptProjectionContent::SessionStep(SessionStepPayload {
                    op: SessionStepProjection::SendDone { .. },
                    ..
                })
            )
        })
        .collect();

    let tables = ContextRefTables::new();
    let ref_table = get_or_create_ref_table(&tables, &ctx_key);
    let registry = discover_stub::registry();
    let history = project_prompt_context(seed_items, &registry, &ref_table, None);

    assert!(
        history.as_array().expect("array").is_empty(),
        "SendDone replay seeding must not emit transcript rows: {history}"
    );
    assert!(
        ref_table.get(ShortRef::new(8)).is_some(),
        "delegated graph replay must seed the current delegated context ref table"
    );

    let read_history = project_prompt_context(
        vec![PromptProjectionItem {
            timestamp_ms: 3,
            activity_anchor: "delegated-page-read".to_string(),
            role: "tool".to_string(),
            content: PromptProjectionContent::SessionStep(SessionStepPayload {
                tool_name,
                op: SessionStepProjection::PageRead {
                    archive_ref: "@8".to_string(),
                    offset: 0,
                    limit: 20,
                },
                send_done_replay_payload: None,
                read_replay_lines: None,
            }),
        }],
        &registry,
        &ref_table,
        None,
    );
    let rows = read_history.as_array().expect("array");
    let content = rows[0]["content"].as_str().expect("content");
    assert!(content.contains("cat -n @8"), "{content}");
    assert!(
        content.contains("counterpoint found in delegated archive"),
        "{content}"
    );
}
