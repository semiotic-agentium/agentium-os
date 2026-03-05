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
use tokio::sync::Mutex;

type ScriptedTurns = Arc<Mutex<VecDeque<Vec<std::result::Result<Message, String>>>>>;

struct MockSource {
    scripted_turns: ScriptedTurns,
    requests: Arc<Mutex<Vec<ClaudeTurnRequest>>>,
}

#[async_trait]
impl ClaudeStreamSource for MockSource {
    async fn stream_turn(
        &self,
        request: ClaudeTurnRequest,
    ) -> std::result::Result<ClaudeMessageStream, baml_rt_tools::ToolSessionError> {
        self.requests.lock().await.push(request);
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

#[derive(Clone)]
struct SharedMockSourceFactory {
    scripted_turns: ScriptedTurns,
    requests: Arc<Mutex<Vec<ClaudeTurnRequest>>>,
}

impl SharedMockSourceFactory {
    fn new() -> Self {
        Self {
            scripted_turns: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn push_turn(&self, events: Vec<std::result::Result<Message, String>>) {
        self.scripted_turns.lock().await.push_back(events);
    }

    async fn requests(&self) -> Vec<ClaudeTurnRequest> {
        self.requests.lock().await.clone()
    }
}

impl ClaudeStreamSourceFactory for SharedMockSourceFactory {
    fn create(
        &self,
        _cwd: PathBuf,
    ) -> std::result::Result<Arc<dyn ClaudeStreamSource>, baml_rt_tools::ToolSessionError> {
        Ok(Arc::new(MockSource {
            scripted_turns: self.scripted_turns.clone(),
            requests: self.requests.clone(),
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
async fn claude_session_suspends_then_resumes_to_done() {
    let source_factory = Arc::new(SharedMockSourceFactory::new());
    source_factory
        .push_turn(vec![
            Ok(assistant_text("first", "sdk-session-1")),
            Ok(result("awaiting_input", "sdk-session-1")),
        ])
        .await;
    source_factory
        .push_turn(vec![
            Ok(assistant_text("second", "sdk-session-1")),
            Ok(result("query_complete", "sdk-session-1")),
        ])
        .await;

    let workspace_registry = Arc::new(AgentWorkspaceRegistry::new(
        tempfile::tempdir().expect("tempdir").path().to_path_buf(),
    ));
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_bundle(ClaudeSessionBundle::with_factory(
            workspace_registry,
            source_factory.clone(),
        ))
        .expect("register claude bundle");

    let scope = baml_rt_core::context::InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000111").expect("uuid"),
    ));
    context::with_scope(scope.as_scope().clone(), async {
        let session_id = registry
            .open_session(
                "claude/dev",
                serde_json::json!({}),
                scope.as_scope().context_id(),
                scope.as_scope().agent_id(),
            )
            .await
            .expect("open");
        registry
            .session_send(&session_id, serde_json::json!({ "prompt": "hello" }))
            .await
            .expect("send #1");

        let suspended = next_until_suspended_or_done(&registry, &session_id).await;
        match suspended {
            ToolStep::Suspended { output } => {
                assert_eq!(output["completion"], "INPUT_REQUIRED");
            }
            other => panic!("expected Suspended, got {other:?}"),
        }

        registry
            .session_send(&session_id, serde_json::json!({ "prompt": "continue" }))
            .await
            .expect("send #2");
        let done = next_until_suspended_or_done(&registry, &session_id).await;
        match done {
            ToolStep::Done {
                output: Some(output),
            } => {
                assert_eq!(output["completion"], "DONE");
            }
            other => panic!("expected Done(Some(_)), got {other:?}"),
        }
    })
    .await;

    let requests = source_factory.requests().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].sdk_session_id, None);
    assert_eq!(
        requests[1].sdk_session_id.as_deref(),
        Some("sdk-session-1"),
        "second turn must resume same sdk session id"
    );
}

#[tokio::test]
async fn claude_session_rejects_double_send_while_active() {
    let source_factory = Arc::new(SharedMockSourceFactory::new());
    source_factory
        .push_turn(vec![
            Ok(assistant_text("streaming", "sdk-session-2")),
            Ok(result("query_complete", "sdk-session-2")),
        ])
        .await;
    let workspace_registry = Arc::new(AgentWorkspaceRegistry::new(
        tempfile::tempdir().expect("tempdir").path().to_path_buf(),
    ));
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_bundle(ClaudeSessionBundle::with_factory(
            workspace_registry,
            source_factory,
        ))
        .expect("register claude bundle");

    let scope = baml_rt_core::context::InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000112").expect("uuid"),
    ));
    context::with_scope(scope.as_scope().clone(), async {
        let session_id = registry
            .open_session(
                "claude/dev",
                serde_json::json!({}),
                scope.as_scope().context_id(),
                scope.as_scope().agent_id(),
            )
            .await
            .expect("open");
        registry
            .session_send(&session_id, serde_json::json!({ "prompt": "hello" }))
            .await
            .expect("first send ok");

        let second_send = registry
            .session_send(
                &session_id,
                serde_json::json!({ "prompt": "invalid second send" }),
            )
            .await;
        assert!(
            second_send.is_err(),
            "double send must fail while active turn exists"
        );
    })
    .await;
}

#[tokio::test]
async fn claude_session_unknown_id_returns_error() {
    let registry = ToolRegistry::new();
    let unknown = ToolSessionId::random();
    let err = registry
        .session_next(&unknown)
        .await
        .expect_err("must error");
    let err_msg = err.to_string();
    let typed_not_found = matches!(
        err,
        baml_rt_core::BamlRtError::SessionLifecycle(
            baml_rt_core::SessionLifecycleError::ToolSessionNotFound { .. }
        )
    );
    let legacy_text = err_msg.contains("Unknown session");
    let new_text = err_msg.contains("Tool session not found");
    assert!(
        typed_not_found || legacy_text || new_text,
        "error should mention unknown session id or be typed not-found; got: {err_msg}"
    );
}
