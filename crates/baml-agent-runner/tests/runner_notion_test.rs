#![cfg(feature = "notion")]

mod common;

use std::{fs, path::PathBuf, sync::Arc};

use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceWriter, SurrealProvenanceStore, SurrealStoreBuilder,
};
use baml_tools_notion::NotionTool;
use common::{
    RunningHttpServer, TempDirCleanup, TempEnvVar, build_notion_agent_to_temp_async, contains_kv,
    e2e_serial_gate, post_a2a_sse_collect, start_http_server, start_runner_api_server,
};
use serde_json::{Value, json};
use test_support::common::{chunks_from_responses, send_stream_request, workspace_fnox_path};
use tokio::time::{Duration, sleep, timeout};

const RAW_BLOCK_ID: &str = "11111111111111111111111111111111";
const NORMALIZED_BLOCK_ID: &str = "11111111-1111-1111-1111-111111111111";
const NORMALIZED_PAGE_ID: &str = "22222222-2222-2222-2222-222222222222";
const NOTION_API_PREFIX: &str = "/v1";

fn notion_api_path(suffix: &str) -> String {
    format!("{NOTION_API_PREFIX}{suffix}")
}

#[derive(Clone, Default)]
struct MockNotionState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockNotionState {
    async fn push_hit(&self, entry: String) {
        self.hits.lock().await.push(entry);
    }

    async fn snapshot(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

async fn start_notion_mock_server() -> std::io::Result<(RunningHttpServer, MockNotionState)> {
    use std::collections::HashMap;

    use axum::{
        Json, Router,
        extract::{Path as AxumPath, Query as AxumQuery, State as AxumState},
        routing::{get, post},
    };

    async fn search_pages(AxumState(state): AxumState<MockNotionState>) -> Json<Value> {
        state
            .push_hit(format!("POST {}", notion_api_path("/search")))
            .await;
        Json(json!({
            "object": "list",
            "results": [
                {
                    "object": "page",
                    "id": NORMALIZED_PAGE_ID,
                    "url": "https://notion.so/agent-platform-roadmap",
                    "last_edited_time": "2026-02-19T00:00:00.000Z",
                    "properties": {
                        "Name": {
                            "type": "title",
                            "title": [{ "plain_text": "Agent Platform Roadmap" }]
                        }
                    }
                }
            ],
            "next_cursor": null,
            "has_more": false
        }))
    }

    async fn get_page(
        AxumState(state): AxumState<MockNotionState>,
        AxumPath(page_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .push_hit(format!("GET {NOTION_API_PREFIX}/pages/{page_id}"))
            .await;
        Json(json!({
            "object": "page",
            "id": page_id,
            "url": "https://notion.so/agent-platform-roadmap",
            "last_edited_time": "2026-02-19T00:00:00.000Z",
            "properties": {
                "Name": {
                    "type": "title",
                    "title": [{ "plain_text": "Agent Platform Roadmap" }]
                }
            }
        }))
    }

    async fn get_blocks(
        AxumState(state): AxumState<MockNotionState>,
        AxumPath(block_id): AxumPath<String>,
        AxumQuery(query): AxumQuery<HashMap<String, String>>,
    ) -> Json<Value> {
        let start_cursor = query.get("start_cursor").cloned();
        state
            .push_hit(match start_cursor.as_deref() {
                Some(cursor) => format!(
                    "GET {NOTION_API_PREFIX}/blocks/{block_id}/children?start_cursor={cursor}"
                ),
                None => format!("GET {NOTION_API_PREFIX}/blocks/{block_id}/children"),
            })
            .await;
        let page = if start_cursor.is_none() {
            json!({
                "object": "list",
                "results": [
                    {
                        "object": "block",
                        "id": block_id,
                        "type": "heading_2",
                        "heading_2": { "rich_text": [] },
                        "has_children": false,
                        "parent": { "type": "page_id", "page_id": NORMALIZED_PAGE_ID }
                    }
                ],
                "next_cursor": "cursor-2",
                "has_more": true
            })
        } else {
            json!({
                "object": "list",
                "results": [
                    {
                        "object": "block",
                        "id": "33333333-3333-3333-3333-333333333333",
                        "type": "bulleted_list_item",
                        "bulleted_list_item": { "rich_text": [] },
                        "has_children": false,
                        "parent": { "type": "page_id", "page_id": NORMALIZED_PAGE_ID }
                    }
                ],
                "next_cursor": null,
                "has_more": false
            })
        };
        Json(page)
    }

    let state = MockNotionState::default();
    let app = Router::new()
        .route(&notion_api_path("/search"), post(search_pages))
        .route(&notion_api_path("/pages/{page_id}"), get(get_page))
        .route(
            &notion_api_path("/blocks/{block_id}/children"),
            get(get_blocks),
        )
        .with_state(state.clone());

    let server = start_http_server(app)
        .await?
        .with_base_path(NOTION_API_PREFIX);

    Ok((server, state))
}

async fn setup_notion_agent_with_provenance()
-> (baml_rt::A2aAgent, Arc<SurrealProvenanceStore>, PathBuf) {
    let built = build_notion_agent_to_temp_async().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("notion built path utf8"))
        .expect("load notion schema");
    manager
        .register_tool(NotionTool::new())
        .await
        .expect("register notion tool");

    let provenance = build_surreal_test_store().await;
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("notion-agent").expect("agent type"),
            "1.0.0".to_string(),
            "notion-agent@1.0.0".to_string(),
        ))
        .await
        .expect("write AgentBooted");

    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("notion-agent dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_surreal_store(provenance.clone())
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build notion agent");
    (agent, provenance, built)
}

#[tokio::test]
async fn test_e2e_notion_direct_id_path_with_mock_server_and_mermaid_http() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (mock_server, mock_state) = match start_notion_mock_server().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Skipping notion direct-id e2e test: cannot bind mock server: {err}");
            return;
        }
    };
    let _env_notion_token = TempEnvVar::set("NOTION_API_TOKEN", "secret_mock_notion_for_test");
    let _env_notion_base = TempEnvVar::set("NOTION_API_BASE_URL", &mock_server.base_url);

    let (agent, provenance_reader, built_dir) = setup_notion_agent_with_provenance().await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);
    let runner_api = match start_runner_api_server("notion-agent", agent, provenance_reader.clone())
        .await
    {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Skipping notion direct-id e2e test: cannot bind runner API server: {err}");
            return;
        }
    };

    let context_id = ContextId::new(88, 7);
    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request_body = send_stream_request(
        "notion-vox-directid-1",
        &format!("Pull details for this notion block {RAW_BLOCK_ID}"),
        correlation_id.as_str(),
        Some(context_id.clone()),
    );

    let http_client = reqwest::Client::new();
    let a2a_url = format!(
        "{}/agents/notion-agent/default/a2a/sse",
        runner_api.base_url
    );
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
    assert!(
        chunks.iter().any(|chunk| !chunk.is_null()),
        "Expected at least one non-null stream chunk. Raw: {}",
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );

    let mut matched_tool_result: Option<Value> = None;
    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut last_signature = String::new();
    let mut stagnant_polls = 0u32;
    for _ in 0..80 {
        let items = provenance_reader
            .conversation_context(&context_id, Some(120))
            .await
            .unwrap_or_default();
        conversation_items = items.clone();
        matched_tool_result = items
            .iter()
            .filter(|item| item.source == "tool_result")
            .find_map(|item| {
                let blocks = item
                    .content
                    .get("result")
                    .and_then(|result| result.get("blocks"))
                    .and_then(Value::as_array)?;
                if !blocks.is_empty() {
                    Some(item.content.clone())
                } else {
                    None
                }
            });
        if matched_tool_result.is_some() {
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

    let tool_result = matched_tool_result.unwrap_or_else(|| {
        panic!(
            "Expected notion tool_result with non-empty blocks. Sources seen: {:?}",
            conversation_items
                .iter()
                .map(|i| i.source.as_str())
                .collect::<Vec<_>>()
        )
    });
    let blocks = tool_result
        .get("result")
        .and_then(|r| r.get("blocks"))
        .and_then(Value::as_array)
        .expect("tool_result.result.blocks array");
    assert!(
        !blocks.is_empty(),
        "Expected at least one block in tool result"
    );

    let has_notion_tool_call = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("name"))
                .and_then(Value::as_str)
                == Some("support/notion")
    });
    assert!(
        has_notion_tool_call,
        "Expected at least one support/notion tool_call item in provenance context"
    );

    let block_id_call_seen = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("args"))
                .map(|args| contains_kv(args, "block_id", RAW_BLOCK_ID))
                .unwrap_or(false)
    });
    assert!(
        block_id_call_seen,
        "Expected at least one notion tool call with block_id={RAW_BLOCK_ID}"
    );

    let mock_hits = mock_state.snapshot().await;
    assert!(
        mock_hits
            .iter()
            .any(|hit| hit == &format!("GET /v1/blocks/{NORMALIZED_BLOCK_ID}/children")),
        "Expected mock Notion block-children endpoint hit. hits={mock_hits:?}"
    );
    assert!(
        mock_hits.iter().any(|hit| hit
            == &format!("GET /v1/blocks/{NORMALIZED_BLOCK_ID}/children?start_cursor=cursor-2")),
        "Expected mock Notion pagination follow-up hit. hits={mock_hits:?}"
    );
    assert!(
        mock_hits
            .iter()
            .any(|hit| hit == &format!("GET /v1/pages/{NORMALIZED_PAGE_ID}")),
        "Expected mock Notion page endpoint hit. hits={mock_hits:?}"
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

    // Structural assertions instead of a full snapshot: participant names, key function
    // calls, and task completion marker must be present, but exact token counts, drift
    // scores, call counts, and agent-response text are all model-dependent and are not
    // asserted here.
    assert!(
        mermaid.contains("sequenceDiagram"),
        "Expected sequenceDiagram header in Mermaid output"
    );
    assert!(
        mermaid.contains("InferNotionIntent"),
        "Expected InferNotionIntent call in Mermaid output"
    );
    assert!(
        mermaid.contains("PlanNotionWork"),
        "Expected PlanNotionWork call in Mermaid output"
    );
    assert!(
        mermaid.contains("ChooseNotionAction"),
        "Expected at least one ChooseNotionAction call in Mermaid output"
    );
    assert!(
        mermaid.contains("ReactToNotionResults"),
        "Expected ReactToNotionResults call in Mermaid output"
    );
    assert!(
        mermaid.contains("\"Completed\""),
        "Expected Completed task section in Mermaid output"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}

async fn build_surreal_test_store() -> Arc<SurrealProvenanceStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build isolated SurrealDB store")
}

#[cfg(feature = "llm-tests")]
#[tokio::test]
async fn test_e2e_notion_real_model_search_with_mock_server() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        eprintln!("Skipping notion LLM test: OPENROUTER_API_KEY not set");
        return;
    }

    let (mock_server, mock_state) = match start_notion_mock_server().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Skipping notion LLM test: cannot bind mock server: {err}");
            return;
        }
    };
    let _env_notion_token = TempEnvVar::set("NOTION_API_TOKEN", "secret_mock_notion_for_test");
    let _env_notion_base = TempEnvVar::set("NOTION_API_BASE_URL", &mock_server.base_url);

    let (agent, provenance_reader, built_dir) = setup_notion_agent_with_provenance().await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);
    let runner_api =
        match start_runner_api_server("notion-agent", agent, provenance_reader.clone()).await {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Skipping notion LLM test: cannot bind runner API server: {err}");
                return;
            }
        };

    let context_id = ContextId::new(88, 8);
    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request_body = send_stream_request(
        "notion-vox-search-1",
        "What are we working on right now? Search Notion and list pages with source links.",
        correlation_id.as_str(),
        Some(context_id.clone()),
    );

    let http_client = reqwest::Client::new();
    let a2a_url = format!(
        "{}/agents/notion-agent/default/a2a/sse",
        runner_api.base_url
    );
    let responses: Vec<Value> = timeout(
        Duration::from_secs(180),
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
    assert!(
        chunks.iter().any(|chunk| !chunk.is_null()),
        "Expected at least one non-null stream chunk. Raw: {}",
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );

    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut last_signature = String::new();
    let mut stagnant_polls = 0u32;
    for _ in 0..80 {
        let items = provenance_reader
            .conversation_context(&context_id, Some(160))
            .await
            .unwrap_or_default();
        conversation_items = items.clone();
        let saw_search_result = items.iter().any(|item| {
            item.source == "tool_result"
                && item
                    .content
                    .get("result")
                    .and_then(|r| r.get("pages"))
                    .and_then(Value::as_array)
                    .map(|pages| !pages.is_empty())
                    .unwrap_or(false)
        });
        if saw_search_result {
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

    let search_call_seen = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("name"))
                .and_then(Value::as_str)
                == Some("support/notion")
    });
    assert!(
        search_call_seen,
        "Expected at least one support/notion tool call in provenance"
    );

    let mock_hits = mock_state.snapshot().await;
    assert!(
        mock_hits.iter().any(|hit| hit == "POST /v1/search"),
        "Expected mock Notion search endpoint hit. hits={mock_hits:?}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}
