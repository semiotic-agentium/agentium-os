//! **Spec regression:** `ctx.tags['conversation_history']` from the same pipeline as
//! [`baml_rt_a2a::a2a_transport::ProjectingConversationContextProvider`]: store rows →
//! [`baml_rt_conversation::provenance_item_to_projection_item`] →
//! [`baml_rt_tools::prompt_projection::project_prompt_context`] with **default** options and a
//! **stub** [`baml_rt_tools::ToolRegistry`] (registers `system/discover_agents` so
//! `describe_invocation` is non-empty — matches integration tests that wire a real catalog).
//!
//! Surface: **Message** (`#N`), **ToolCall** + **ToolResult** (execute), **SessionStep** (Open,
//! SendDone, SearchRead, PageRead), **ContextRefTables** + **ArchiveReader** (live grep/cat).
//! Normative: [`docs/baml-rt-conversation-spec.md`]. Update: `INSTA_UPDATE=1 cargo test -p baml-rt-a2a --test conversation_history_snapshot`

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

use std::time::Duration;

use baml_rt_conversation::view::SessionStepOp;
use baml_rt_core::{
    Outcome,
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, UuidId},
};
use baml_rt_provenance::{
    CallScope, ProvEvent, ProvenanceContextReader, ProvenanceWriter, SurrealStoreBuilder,
};
use baml_rt_tools::{
    archive_read::{
        GrepPattern, LineOffset, PageLimit, ShortRef, format_grep_page_as_session_read_body,
        grep_paginate, render_to_lines,
    },
    archive_refs::{ArchiveEntry, ContextRefTables, get_or_create_ref_table},
    prompt_projection::project_prompt_context,
};

fn make_archive_reader(
    tables: ContextRefTables,
    context_id: String,
) -> impl Fn(&str, Option<&str>, usize, usize) -> Option<String> {
    move |archive_ref_str, grep_str, offset, limit| {
        let short_ref = ShortRef::parse(archive_ref_str)?;
        let ref_table = baml_rt_tools::archive_refs::get_ref_table(&tables, &context_id)?;
        let entry = ref_table.get(short_ref)?;
        let grep = grep_str
            .filter(|s| !s.is_empty())
            .and_then(|s| GrepPattern::parse(s).ok());
        let page = grep_paginate(
            &entry.content,
            grep.as_ref(),
            LineOffset(offset),
            PageLimit::new(limit),
        );
        Some(format_grep_page_as_session_read_body(
            &page,
            archive_ref_str,
            grep_str,
        ))
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

    let tables: ContextRefTables = ContextRefTables::new();
    let ref_table = get_or_create_ref_table(&tables, &ctx_key);
    let lines_content = render_to_lines(&result_payload);
    let entry = ArchiveEntry::new(
        lines_content,
        tool_name.clone(),
        "found 2 agents".into(),
        String::new(),
        "tool_result".to_string(),
    );
    let short_at1 = ShortRef::new(1);
    ref_table.insert_virtual_archive(1, entry.clone());
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
    let reader = make_archive_reader(tables, ctx_key);
    let history = project_prompt_context(projection_items, &registry, &ref_table, Some(&reader));

    insta::assert_json_snapshot!(history);
}
