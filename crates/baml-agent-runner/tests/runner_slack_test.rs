#![cfg(feature = "slack")]

mod common;

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphqliteProvenanceStore, GraphqliteStoreBuilder, ProvEvent,
    ProvenanceContextReader, ProvenanceConversationContextItem, ProvenanceWriter,
};
use baml_tools_slack::SlackTool;
use common::{
    RunningHttpServer, TempDirCleanup, TempEnvVar, build_slack_agent_to_temp_async, contains_kv,
    e2e_serial_gate, post_a2a_sse_collect, start_http_server, start_runner_api_server,
};
use serde_json::{Value, json};
use test_support::common::{
    chunks_from_responses, message_texts_from_chunks, send_stream_request, workspace_fnox_path,
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
        .route("/api/conversations.replies", get(thread_replies))
        .route("/api/users.info", get(users_info))
        .with_state(state.clone());
    let server = start_http_server(app).await?.with_base_path("/api");
    Ok((server, state))
}

async fn setup_slack_agent_with_provenance()
-> (baml_rt::A2aAgent, Arc<GraphqliteProvenanceStore>, PathBuf) {
    let built = build_slack_agent_to_temp_async().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("slack built path utf8"))
        .expect("load slack schema");
    manager
        .register_tool(SlackTool::new())
        .await
        .expect("register slack tool");

    let provenance = build_graphqlite_test_store();
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
        .with_graphqlite_store(provenance.clone())
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build slack agent");
    (agent, provenance, built)
}

fn build_graphqlite_test_store() -> Arc<GraphqliteProvenanceStore> {
    let path = std::env::temp_dir().join(format!(
        "baml-rt-runner-slack-{pid}-{unique}.db",
        pid = std::process::id(),
        unique = uuid::Uuid::new_v4(),
    ));
    GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build isolated GraphQLite store")
}

#[tokio::test]
async fn test_e2e_slack_todo_extraction_with_mock_server_and_mermaid_http() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (mock_server, mock_state) = match start_slack_mock_server().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Skipping slack e2e test: cannot bind mock server: {err}");
            return;
        }
    };
    let _env_slack_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb_mock_slack_for_test");
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
    assert!(
        merged_text.contains("Action items"),
        "Expected todo extraction text. Full text: {merged_text}"
    );
    assert!(
        merged_text.contains("Confidence"),
        "Expected confidence field in agent response. Full text: {merged_text}"
    );
    assert!(
        merged_text.contains("slack://channel/C12345678/p1735689600000000"),
        "Expected source reference in agent response. Full text: {merged_text}"
    );

    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut saw_tool_result = false;
    let mut last_signature = String::new();
    let mut stagnant_polls = 0u32;
    for _ in 0..80 {
        let items = provenance_reader
            .conversation_context(&context_id, Some(120))
            .await
            .unwrap_or_default();
        conversation_items = items.clone();
        saw_tool_result = items.iter().any(|item| {
            item.source == "tool_result"
                && item
                    .content
                    .get("result")
                    .and_then(|result| result.get("messages"))
                    .and_then(Value::as_array)
                    .map(|messages| !messages.is_empty())
                    .unwrap_or(false)
        });
        if saw_tool_result {
            break;
        }
        let signature = serde_json::to_string(
            &conversation_items
                .iter()
                .map(|i| (&i.event_id, &i.source, &i.content))
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
        "Expected non-empty support/slack tool_result in provenance context"
    );

    let has_slack_tool_call = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tool_call| tool_call.get("name"))
                .and_then(Value::as_str)
                == Some("support/slack")
    });
    assert!(
        has_slack_tool_call,
        "Expected at least one support/slack tool_call item in provenance context"
    );

    let saw_channel_id = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tool_call| tool_call.get("args"))
                .map(|args| contains_kv(args, "channel_id", "C12345678"))
                .unwrap_or(false)
    });
    assert!(
        saw_channel_id,
        "Expected Slack tool call args to include channel_id=C12345678"
    );

    let saw_thread_ts = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tool_call| tool_call.get("args"))
                .map(|args| contains_kv(args, "thread_ts", "1735689600.000000"))
                .unwrap_or(false)
    });
    assert!(
        saw_thread_ts,
        "Expected Slack tool call args to include thread_ts=1735689600.000000"
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

    let mermaid_url = format!(
        "{}/contexts/{}/mermaid",
        runner_api.base_url,
        context_id.as_str()
    );
    let mermaid_response = timeout(Duration::from_secs(20), http_client.get(mermaid_url).send())
        .await
        .expect("mermaid request timed out")
        .expect("mermaid request failed");
    assert!(
        mermaid_response.status().is_success(),
        "Expected 200 from /contexts/<context_id>/mermaid, got {}",
        mermaid_response.status()
    );
    let mermaid = mermaid_response.text().await.expect("mermaid body");
    assert!(
        mermaid.contains("sequenceDiagram"),
        "Expected mermaid sequence output, got: {mermaid}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}
