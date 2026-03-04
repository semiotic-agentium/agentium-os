use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    context,
    ids::{AgentId, UuidId},
};
use baml_rt_tools::{ToolRegistry, ToolSessionId, ToolStep};
use baml_rt_tools_claude::{
    AgentWorkspaceRegistry, ClaudeMessageStream, ClaudeSessionBundle, ClaudeStreamSource,
    ClaudeStreamSourceFactory, ClaudeTurnRequest,
};
use claude_agent_sdk_rs::{
    AssistantMessage, AssistantMessageInner, ContentBlock, Message, ResultMessage, TextBlock,
};
use futures_util::stream;
use serde_json::json;
use tokio::sync::Mutex;

type ScriptedTurns = Arc<Mutex<VecDeque<Vec<std::result::Result<Message, String>>>>>;

#[derive(Clone)]
struct SharedMockSourceFactory {
    scripted_turns: ScriptedTurns,
}

impl SharedMockSourceFactory {
    fn new() -> Self {
        Self {
            scripted_turns: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    async fn push_turn(&self, events: Vec<std::result::Result<Message, String>>) {
        self.scripted_turns.lock().await.push_back(events);
    }
}

struct MockSource {
    scripted_turns: ScriptedTurns,
}

#[async_trait]
impl ClaudeStreamSource for MockSource {
    async fn stream_turn(
        &self,
        _request: ClaudeTurnRequest,
    ) -> std::result::Result<ClaudeMessageStream, baml_rt_tools::ToolSessionError> {
        let scripted = self
            .scripted_turns
            .lock()
            .await
            .pop_front()
            .unwrap_or_default();
        Ok(Box::pin(stream::iter(scripted)))
    }

    async fn shutdown(&self) -> std::result::Result<(), baml_rt_tools::ToolSessionError> {
        Ok(())
    }
}

impl ClaudeStreamSourceFactory for SharedMockSourceFactory {
    fn create(
        &self,
        _cwd: PathBuf,
    ) -> std::result::Result<Arc<dyn ClaudeStreamSource>, baml_rt_tools::ToolSessionError> {
        Ok(Arc::new(MockSource {
            scripted_turns: self.scripted_turns.clone(),
        }))
    }
}

fn assistant_text(text: &str, session_id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        message: AssistantMessageInner {
            content: vec![ContentBlock::Text(TextBlock {
                text: text.to_string(),
            })],
            model: None,
            id: None,
            stop_reason: None,
            usage: None,
            error: None,
        },
        parent_tool_use_id: None,
        session_id: Some(session_id.to_string()),
        uuid: None,
    })
}

fn result(subtype: &str, session_id: &str) -> Message {
    Message::Result(ResultMessage {
        subtype: subtype.to_string(),
        duration_ms: 10,
        duration_api_ms: 5,
        is_error: false,
        num_turns: 1,
        session_id: session_id.to_string(),
        total_cost_usd: None,
        usage: None,
        result: None,
        structured_output: None,
    })
}

async fn next_until_suspended_or_done(
    registry: &ToolRegistry,
    session_id: &ToolSessionId,
) -> ToolStep {
    for _ in 0..16 {
        match registry
            .session_next(session_id)
            .await
            .expect("session_next")
        {
            ToolStep::Streaming { .. } => continue,
            step => return step,
        }
    }
    panic!("expected Suspended or Done within bounded next() calls");
}

#[tokio::test]
async fn claude_bundle_exposes_claude_dev_metadata() {
    let registry = Arc::new(ToolRegistry::new());
    let workspace_registry = Arc::new(AgentWorkspaceRegistry::new(
        tempfile::tempdir().expect("tempdir").path().to_path_buf(),
    ));
    let source_factory = Arc::new(SharedMockSourceFactory::new());
    registry
        .register_bundle(ClaudeSessionBundle::with_factory(
            workspace_registry,
            source_factory,
        ))
        .expect("register claude bundle");

    let metadata = registry
        .get_metadata("claude/dev")
        .expect("claude/dev metadata");
    assert_eq!(metadata.name.to_string(), "claude/dev");
}

#[tokio::test]
async fn claude_dev_fsm_suspend_then_done_cycle() {
    let source_factory = Arc::new(SharedMockSourceFactory::new());
    source_factory
        .push_turn(vec![
            Ok(assistant_text("first", "sdk-session-3")),
            Ok(result("awaiting_input", "sdk-session-3")),
        ])
        .await;
    source_factory
        .push_turn(vec![
            Ok(assistant_text("second", "sdk-session-3")),
            Ok(result("query_complete", "sdk-session-3")),
        ])
        .await;

    let registry = Arc::new(ToolRegistry::new());
    let workspace_registry = Arc::new(AgentWorkspaceRegistry::new(
        tempfile::tempdir().expect("tempdir").path().to_path_buf(),
    ));
    registry
        .register_bundle(ClaudeSessionBundle::with_factory(
            workspace_registry,
            source_factory,
        ))
        .expect("register claude bundle");

    let scope = baml_rt_core::context::InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000113").expect("uuid"),
    ));
    context::with_scope(scope.as_scope().clone(), async {
        let session_id = registry
            .open_session(
                "claude/dev",
                json!({}),
                scope.as_scope().context_id(),
                scope.as_scope().agent_id(),
            )
            .await
            .expect("open");
        registry
            .session_send(&session_id, json!({ "prompt": "hello" }))
            .await
            .expect("send");
        let first = next_until_suspended_or_done(&registry, &session_id).await;
        assert!(matches!(first, ToolStep::Suspended { .. }));

        registry
            .session_send(&session_id, json!({ "prompt": "continue" }))
            .await
            .expect("send2");
        let second = next_until_suspended_or_done(&registry, &session_id).await;
        assert!(matches!(second, ToolStep::Done { .. }));
    })
    .await;
}
