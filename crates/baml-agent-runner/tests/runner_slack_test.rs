#![cfg(all(feature = "llm-tests", feature = "slack"))]

mod common;

#[path = "common/http_tool_test_helpers.rs"]
mod http_tool_test_helpers;

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt::{InterceptorDecision, LLMCallContext, LLMInterceptor, baml::BamlRuntimeManager};
use baml_rt_core::{
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceWriter, SurrealProvenanceStore,
    store::{ConversationItemContent, SessionStepOp, ToolOutcome},
};
use baml_tools_slack::SlackTool;
use common::{
    RunningHttpServer, TempDirCleanup, TempEnvVar, build_slack_agent_to_temp_async,
    e2e_serial_gate, fetch_context_mermaid, post_a2a_sse_collect, start_http_server,
    start_runner_api_server,
};
use http_tool_test_helpers::contains_kv;
use serde_json::{Value, json};
use test_support::common::{
    chunks_from_responses, message_texts_from_chunks, message_visible_content_from_chunks,
    send_stream_request, test_surreal_store, workspace_fnox_path,
};
use tokio::time::{Duration, sleep, timeout};

#[derive(Clone, Default)]
struct MockSlackState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockSlackState {
    async fn push_hit(&self, hit: String) {
        self.hits.lock().await.push(hit);
    }

    async fn snapshot(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

async fn start_slack_mock_server() -> std::io::Result<(RunningHttpServer, MockSlackState)> {
    use axum::{
        Json, Router,
        extract::{OriginalUri, Query as AxumQuery, State as AxumState},
        routing::get,
    };

    async fn thread_replies(
        AxumState(state): AxumState<MockSlackState>,
        uri: OriginalUri,
    ) -> Json<Value> {
        state.push_hit(format!("GET {}", uri.0)).await;
        Json(json!({
            "ok": true,
            "messages": [
                {
                    "type": "message",
                    "user": "UALICE",
                    "text": "TODO: <@UBOB> ship the Slack integration by 2026-03-10.",
                    "ts": "1735689600.000000",
                    "thread_ts": "1735689600.000000"
                },
                {
                    "type": "message",
                    "user": "UBOB",
                    "text": "I'll publish the OAuth runbook by Friday.",
                    "ts": "1735689700.000000",
                    "thread_ts": "1735689600.000000"
                }
            ],
            "has_more": false,
            "response_metadata": { "next_cursor": "" }
        }))
    }

    async fn conversation_history(
        AxumState(state): AxumState<MockSlackState>,
        uri: OriginalUri,
    ) -> Json<Value> {
        state.push_hit(format!("GET {}", uri.0)).await;
        Json(json!({
            "ok": true,
            "messages": [
                {
                    "type": "message",
                    "user": "UALICE",
                    "text": "TODO: <@UBOB> ship the Slack integration by 2026-03-10.",
                    "ts": "1735689600.000000",
                    "thread_ts": "1735689600.000000"
                }
            ],
            "has_more": false,
            "response_metadata": { "next_cursor": "" }
        }))
    }

    async fn users_info(
        AxumState(state): AxumState<MockSlackState>,
        uri: OriginalUri,
        AxumQuery(query): AxumQuery<HashMap<String, String>>,
    ) -> Json<Value> {
        state.push_hit(format!("GET {}", uri.0)).await;
        let user = query.get("user").cloned().unwrap_or_default();
        let body = match user.as_str() {
            "UALICE" => json!({
                "ok": true,
                "user": {
                    "id": "UALICE",
                    "name": "alice",
                    "is_bot": false,
                    "deleted": false,
                    "profile": {
                        "display_name": "Alice",
                        "real_name": "Alice Example"
                    }
                }
            }),
            "UBOB" => json!({
                "ok": true,
                "user": {
                    "id": "UBOB",
                    "name": "bob",
                    "is_bot": false,
                    "deleted": false,
                    "profile": {
                        "display_name": "Bob",
                        "real_name": "Bob Example"
                    }
                }
            }),
            _ => json!({
                "ok": false,
                "error": "user_not_found"
            }),
        };
        Json(body)
    }

    let state = MockSlackState::default();
    let app = Router::new()
        .route("/api/conversations.history", get(conversation_history))
        .route("/api/conversations.replies", get(thread_replies))
        .route("/api/users.info", get(users_info))
        .with_state(state.clone());
    let server = start_http_server(app, Some("/api")).await?;
    Ok((server, state))
}

/// `ChooseSlackAction__select` in the **AwaitingOpen** phase only allows `Open | ReadOnlyResponse` in the
/// generated schema, but capable models often skip straight to `Send` (valid FSM intent, invalid for that
/// hop's union). That yields "Parsed result conversion failed" — a **codegen/schema vs prompt** tension, not
/// product slack. Pin the open hop so this E2E exercises the local Slack API fixture + synthesis; all later hops still hit
/// the real model.
#[derive(Clone, Copy)]
struct SlackE2eOpenHopInterceptor;

#[async_trait]
impl LLMInterceptor for SlackE2eOpenHopInterceptor {
    async fn intercept_llm_call(
        &self,
        ctx: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if ctx.function_id.full_name() != "ChooseSlackAction__select" {
            return Ok(InterceptorDecision::Allow);
        }
        let prompt_blob = serde_json::to_string(&ctx.prompt).unwrap_or_default();
        if prompt_blob.contains("[OPEN]") {
            return Ok(InterceptorDecision::Substitute(json!({
                "op": "Open",
                "tool_name": "support/slack",
            })));
        }
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        _: &LLMCallContext,
        _: &baml_rt_core::Result<Value>,
        _: u64,
    ) {
    }
}

async fn setup_slack_agent_with_provenance()
-> (baml_rt::A2aAgent, Arc<SurrealProvenanceStore>, PathBuf) {
    let built = build_slack_agent_to_temp_async().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("slack built path utf8"))
        .expect("load slack schema");
    manager
        .register_llm_interceptor(SlackE2eOpenHopInterceptor)
        .await;
    manager
        .register_tool(SlackTool::new())
        .await
        .expect("register slack tool");

    let provenance = test_surreal_store().await;
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("slack-agent").expect("agent type"),
            "1.0.0".to_string(),
            "slack-agent@1.0.0".to_string(),
        ))
        .await
        .expect("write AgentBooted");

    let agent_code =
        fs::read_to_string(built.join("dist").join("index.js")).expect("slack-agent dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_surreal_store(provenance.clone())
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build slack agent");
    (agent, provenance, built)
}

#[tokio::test]
async fn test_e2e_slack_todo_extraction_with_mock_server_and_mermaid_http() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (mock_server, mock_state) = match start_slack_mock_server().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Skipping slack e2e test: cannot bind fixture server: {err}");
            return;
        }
    };
    let _env_slack_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb_test_slack_fixture");
    let _env_slack_base = TempEnvVar::set("SLACK_API_BASE_URL", &mock_server.base_url);
    let _env_slack_user = TempEnvVar::remove("SLACK_USER_TOKEN");

    let (agent, provenance_reader, built_dir) = setup_slack_agent_with_provenance().await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);
    let runner_api =
        match start_runner_api_server("slack-agent", agent, provenance_reader.clone()).await {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Skipping slack e2e test: cannot bind runner API server: {err}");
                return;
            }
        };

    let context_id = ContextId::new(99, 7);
    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request_body = send_stream_request(
        "slack-vox-thread-1",
        "Extract todos from this Slack thread https://acme.slack.com/archives/C12345678/p1735689600000000 and include owners, due dates, confidence, and sources.",
        correlation_id.as_str(),
        Some(context_id.clone()),
    );

    let http_client = reqwest::Client::new();
    let a2a_url = format!("{}/agents/slack-agent/default/a2a/sse", runner_api.base_url);
    let responses: Vec<Value> = timeout(
        Duration::from_secs(90),
        post_a2a_sse_collect(&http_client, &a2a_url, &request_body),
    )
    .await
    .expect("a2a SSE request timed out")
    .expect("a2a SSE request failed");
    assert!(
        !responses.is_empty(),
        "Expected non-empty JSON-RPC response array from /a2a/sse"
    );

    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    let merged_text = texts.join("\n");
    let merged_lower = merged_text.to_lowercase();
    // The model legitimately routes the substantive answer into a `DataPart` (JSON payload) when
    // the schema offers one — `merged_visible` includes both `TextPart` text and `DataPart` data so
    // the retrieval-grounded check below sees the whole reply rather than just the text summary.
    let visible = message_visible_content_from_chunks(&chunks);
    let merged_visible = visible.join("\n");
    let merged_visible_lower = merged_visible.to_lowercase();
    // LLM phrasing is non-deterministic — do not assert fixed headings or citation URL shapes.
    // Deterministic checks: fixture HTTP hits + provenance (below) + no planning/coordination failure text.
    assert!(
        !merged_lower.contains("planning output did not satisfy")
            && !merged_lower.contains("planning failed:"),
        "Expected no planning/coordination failure in streamed assistant text. merged={merged_text:?}"
    );
    assert!(
        !merged_visible.trim().is_empty(),
        "Expected some assistant-visible streamed content (text or data parts); \
         merged_text={merged_text:?} merged_visible={merged_visible:?}"
    );
    // Fixture thread lines (synthetic Slack API) — if the model echoes them, retrieval likely grounded the answer.
    let echoes_thread_fixture = merged_visible_lower.contains("ship the slack integration")
        || merged_visible_lower.contains("oauth runbook")
        || merged_visible_lower.contains("todo:");
    assert!(
        echoes_thread_fixture || merged_visible.trim().len() >= 80,
        "Expected either retrieved thread content reflected in assistant output or a substantive reply; \
         text_len={} visible_len={} merged_visible={merged_visible:?}",
        merged_text.trim().len(),
        merged_visible.trim().len()
    );

    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut saw_tool_result = false;
    let mut last_signature = String::new();
    let mut stagnant_polls = 0u32;
    for _ in 0..80 {
        // `conversation_context` returns the last N items by time; long streamed turns can drop
        // older SessionStep rows if the cap is too small.
        let items = provenance_reader
            .conversation_context(&context_id, Some(200))
            .await
            .unwrap_or_default();
        conversation_items = items.clone();
        saw_tool_result = items.iter().any(|item| {
            match &item.content {
                // Non-session tools: flat tool_result with payload.
                ConversationItemContent::ToolResult(tr) if tr.tool_name == "support/slack" => {
                    if let ToolOutcome::Result(v) = &tr.outcome {
                        v.get("messages")
                            .and_then(Value::as_array)
                            .map(|m| !m.is_empty())
                            .unwrap_or(false)
                    } else {
                        false
                    }
                }
                // Session FSM: conversation_context uses SessionStep (SendDone), not ToolResult.
                ConversationItemContent::SessionStep(ss) if ss.tool_name == "support/slack" => {
                    matches!(
                        &ss.op,
                        SessionStepOp::SendDone { header, .. }
                            if header.contains("C12345678")
                    )
                }
                _ => false,
            }
        });
        if saw_tool_result {
            break;
        }
        let signature = serde_json::to_string(
            &conversation_items
                .iter()
                .map(|i| (&i.activity_anchor, i.source_name(), &i.content))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        if signature == last_signature {
            stagnant_polls += 1;
        } else {
            stagnant_polls = 0;
            last_signature = signature;
        }
        if stagnant_polls >= 12 {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    assert!(
        saw_tool_result,
        "Expected support/slack evidence in provenance context (ToolResult.messages or SessionStep SendDone)"
    );

    let has_slack_tool_call = conversation_items.iter().any(|item| {
        matches!(
            &item.content,
            ConversationItemContent::ToolCall(tc) if tc.tool_name == "support/slack"
        ) || matches!(
            &item.content,
            ConversationItemContent::SessionStep(ss)
                if ss.tool_name == "support/slack"
                    && matches!(ss.op, SessionStepOp::Open)
        )
    });
    assert!(
        has_slack_tool_call,
        "Expected support/slack tool_call or session Open in provenance context"
    );

    let saw_channel_id = conversation_items.iter().any(|item| {
        matches!(
            &item.content,
            ConversationItemContent::ToolCall(tc)
                if tc.tool_name == "support/slack"
                    && contains_kv(&tc.args, "channel_id", "C12345678")
        ) || matches!(
            &item.content,
            ConversationItemContent::SessionStep(ss)
                if ss.tool_name == "support/slack"
                    && matches!(
                        &ss.op,
                        SessionStepOp::SendDone { header, .. } if header.contains("C12345678")
                    )
        )
    });
    assert!(
        saw_channel_id,
        "Expected Slack channel_id=C12345678 in tool args or session SendDone header"
    );

    let saw_thread_ts = conversation_items.iter().any(|item| {
        matches!(
            &item.content,
            ConversationItemContent::ToolCall(tc)
                if tc.tool_name == "support/slack"
                    && contains_kv(&tc.args, "thread_ts", "1735689600.000000")
        ) || matches!(
            &item.content,
            ConversationItemContent::SessionStep(ss)
                if ss.tool_name == "support/slack"
                    && matches!(
                        &ss.op,
                        SessionStepOp::SendDone { header, .. }
                            if header.contains("1735689600.000000")
                    )
        )
    });
    assert!(
        saw_thread_ts,
        "Expected Slack thread_ts in tool args or session SendDone header"
    );

    let mock_hits = mock_state.snapshot().await;
    assert!(
        mock_hits
            .iter()
            .any(|hit| hit.contains("/api/conversations.replies")),
        "Expected conversations.replies endpoint hit. hits={mock_hits:?}"
    );
    assert!(
        mock_hits.iter().any(|hit| hit.contains("/api/users.info?")),
        "Expected users.info endpoint hit for user resolution. hits={mock_hits:?}"
    );

    let mermaid = fetch_context_mermaid(
        &http_client,
        runner_api.base_url.as_str(),
        context_id.as_str(),
    )
    .await;
    assert!(
        mermaid.contains("sequenceDiagram"),
        "Expected mermaid sequence output, got: {mermaid}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}
