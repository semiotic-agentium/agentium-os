#![cfg(all(feature = "llm-tests", feature = "notion"))]

mod common;

use std::{fs, path::PathBuf, sync::Arc};

use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceWriter, SurrealProvenanceStore,
    store::{ConversationItemContent, SessionStepOp, ToolOutcome},
};
use baml_tools_notion::NotionTool;
use common::{
    RunningHttpServer, TempDirCleanup, TempEnvVar, build_notion_agent_to_temp_async,
    e2e_secs_ci_or_local, e2e_serial_gate, fetch_context_mermaid, post_a2a_sse_collect,
    start_http_server, start_runner_api_server, try_load_dotenv_for_tests,
};
use serde_json::{Value, json};
use test_support::common::{
    chunks_from_responses, send_stream_request, test_surreal_store, workspace_fnox_path,
};
use tokio::time::{Duration, sleep, timeout};

const RAW_BLOCK_ID: &str = "11111111111111111111111111111111";
const NORMALIZED_BLOCK_ID: &str = "11111111-1111-1111-1111-111111111111";
const NORMALIZED_PAGE_ID: &str = "22222222-2222-2222-2222-222222222222";
const NOTION_API_PREFIX: &str = "/v1";

fn notion_direct_id_a2a_collect_timeout() -> Duration {
    Duration::from_secs(e2e_secs_ci_or_local(100, 120))
}

fn notion_api_path(suffix: &str) -> String {
    format!("{NOTION_API_PREFIX}{suffix}")
}

fn notion_mermaid_complete(mermaid: &str) -> bool {
    mermaid.contains("sequenceDiagram")
        && mermaid.contains("InferNotionIntent")
        && mermaid.contains("PlanNotionWork")
        && mermaid.contains("ChooseNotionAction")
        && mermaid.contains("ReactToNotionResults")
        && mermaid.contains("Completed")
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

    async fn clear_hits(&self) {
        self.hits.lock().await.clear();
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

    let server = start_http_server(app, Some(NOTION_API_PREFIX)).await?;

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

    let provenance = test_surreal_store().await;
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
        .with_quickjs_config(
            baml_rt::QuickJSConfig::new()
                .with_stream_collector_idle_secs(Some(e2e_secs_ci_or_local(90, 120))),
        )
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

    let http_client = reqwest::Client::new();
    let a2a_url = format!(
        "{}/agents/notion-agent/default/a2a/sse",
        runner_api.base_url
    );

    let expected_block_children_hit = format!("GET /v1/blocks/{NORMALIZED_BLOCK_ID}/children");

    let mut last_diag: Option<(Vec<String>, Vec<String>)> = None;

    for attempt in 0..1u32 {
        mock_state.clear_hits().await;
        // Fresh context per attempt so provenance reads do not mix failed LLM runs.
        let context_id = ContextId::new(88, 70u64 + u64::from(attempt));
        let correlation_id = baml_rt_core::correlation::generate_correlation_id();
        let message_id = format!("notion-vox-directid-a{attempt}");
        let request_body = send_stream_request(
            &message_id,
            &format!("Pull details for this notion block {RAW_BLOCK_ID}"),
            correlation_id.as_str(),
            Some(context_id.clone()),
        );

        let responses: Vec<Value> = timeout(
            notion_direct_id_a2a_collect_timeout(),
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

        let mut saw_notion_blocks_in_tool_result = false;
        let mut saw_notion_send_done = false;
        let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
        let mut last_signature = String::new();
        let mut stagnant_polls = 0u32;
        for _ in 0..80 {
            let items = provenance_reader
                .conversation_context(&context_id, Some(120))
                .await
                .unwrap_or_else(|e| {
                    panic!(
                        "provenance conversation_context failed (context_id={}): {e}",
                        context_id.as_str()
                    )
                });
            conversation_items = items.clone();
            let mock_hits_poll = mock_state.snapshot().await;
            let poll_hit_blocks = mock_hits_poll
                .iter()
                .any(|hit| hit == &expected_block_children_hit);
            saw_notion_blocks_in_tool_result = items.iter().any(|item| {
                if let ConversationItemContent::ToolResult(tr) = &item.content
                    && tr.tool_name == "support/notion"
                    && let ToolOutcome::Result(v) = &tr.outcome
                {
                    v.get("blocks")
                        .and_then(Value::as_array)
                        .map(|b| !b.is_empty())
                        .unwrap_or(false)
                } else {
                    false
                }
            });
            saw_notion_send_done = items.iter().any(|item| {
                if let ConversationItemContent::SessionStep(ss) = &item.content {
                    ss.tool_name == "support/notion"
                        && matches!(&ss.op, SessionStepOp::SendDone { .. })
                } else {
                    false
                }
            });
            if saw_notion_blocks_in_tool_result || saw_notion_send_done || poll_hit_blocks {
                break;
            }
            let signature = serde_json::to_string(
                &conversation_items
                    .iter()
                    .map(|i| (&i.activity_anchor, i.source_name(), &i.content))
                    .collect::<Vec<_>>(),
            )
            .expect("serialize conversation snapshot for stagnation poll");
            if signature == last_signature {
                stagnant_polls += 1;
            } else {
                stagnant_polls = 0;
                last_signature = signature;
            }
            if stagnant_polls >= 12 {
                let any_notion_http = mock_hits_poll
                    .iter()
                    .any(|h| h.starts_with("GET /v1/") || h.starts_with("POST /v1/"));
                if any_notion_http {
                    break;
                }
                stagnant_polls = 0;
            }
            sleep(Duration::from_millis(250)).await;
        }

        let mock_hits = mock_state.snapshot().await;
        let saw_mock_block_children = mock_hits
            .iter()
            .any(|hit| hit == &expected_block_children_hit);

        let retrieval_ok =
            saw_notion_blocks_in_tool_result || saw_notion_send_done || saw_mock_block_children;
        let sources: Vec<String> = conversation_items
            .iter()
            .map(|i| i.source_name().to_string())
            .collect();
        if !retrieval_ok {
            last_diag = Some((sources, mock_hits.clone()));
            eprintln!(
                "notion direct-id e2e attempt {}: no Notion traffic yet (LLM/plan flake); retrying…",
                attempt + 1
            );
            sleep(Duration::from_secs(2)).await;
            continue;
        }

        let has_notion_in_provenance = conversation_items.iter().any(|item| match &item.content {
            ConversationItemContent::ToolCall(tc) if tc.tool_name == "support/notion" => true,
            ConversationItemContent::SessionStep(ss) if ss.tool_name == "support/notion" => true,
            _ => false,
        });
        if !(has_notion_in_provenance || saw_mock_block_children) {
            last_diag = Some((sources, mock_hits.clone()));
            sleep(Duration::from_secs(2)).await;
            continue;
        }

        assert!(
            mock_hits
                .iter()
                .any(|hit| { hit == &format!("GET /v1/blocks/{NORMALIZED_BLOCK_ID}/children") }),
            "Expected mock Notion block-children endpoint hit. hits={mock_hits:?}"
        );
        assert!(
            mock_hits.iter().any(|hit| {
                hit == &format!(
                    "GET /v1/blocks/{NORMALIZED_BLOCK_ID}/children?start_cursor=cursor-2"
                )
            }),
            "Expected mock Notion pagination follow-up hit. hits={mock_hits:?}"
        );
        assert!(
            mock_hits
                .iter()
                .any(|hit| hit == &format!("GET /v1/pages/{NORMALIZED_PAGE_ID}")),
            "Expected mock Notion page endpoint hit. hits={mock_hits:?}"
        );

        let mut mermaid = String::new();
        for _ in 0..15 {
            mermaid = fetch_context_mermaid(
                &http_client,
                runner_api.base_url.as_str(),
                context_id.as_str(),
            )
            .await;
            if notion_mermaid_complete(&mermaid) {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }

        let mermaid_ok = notion_mermaid_complete(&mermaid);
        if !mermaid_ok {
            last_diag = Some((sources, mock_hits.clone()));
            eprintln!(
                "notion direct-id e2e attempt {}: mermaid incomplete; retrying…",
                attempt + 1
            );
            sleep(Duration::from_secs(2)).await;
            continue;
        }

        runner_api.stop().await;
        mock_server.stop().await;
        return;
    }

    let (src, hits) = last_diag.unwrap_or_else(|| (vec![], vec![]));
    panic!(
        "notion direct-id e2e failed (live LLM/plan flake). sources: {src:?}, mock hits: {hits:?}"
    );
}

#[cfg(feature = "llm-tests")]
#[tokio::test]
async fn test_e2e_notion_real_model_search_with_mock_server() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    try_load_dotenv_for_tests();
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
        Duration::from_secs(300),
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

    let mut saw_pages_in_tool_result = false;
    let mut saw_notion_send_done = false;
    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut last_signature = String::new();
    let mut stagnant_polls = 0u32;
    for _ in 0..80 {
        let items = provenance_reader
            .conversation_context(&context_id, Some(160))
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "provenance conversation_context failed (context_id={}): {e}",
                    context_id.as_str()
                )
            });
        conversation_items = items.clone();
        let mock_hits_poll = mock_state.snapshot().await;
        let poll_hit_search = mock_hits_poll.iter().any(|hit| hit == "POST /v1/search");
        saw_pages_in_tool_result = items.iter().any(|item| {
            if let ConversationItemContent::ToolResult(tr) = &item.content
                && tr.tool_name == "support/notion"
                && let ToolOutcome::Result(v) = &tr.outcome
            {
                v.get("pages")
                    .and_then(Value::as_array)
                    .map(|pages| !pages.is_empty())
                    .unwrap_or(false)
            } else {
                false
            }
        });
        saw_notion_send_done = items.iter().any(|item| {
            if let ConversationItemContent::SessionStep(ss) = &item.content {
                ss.tool_name == "support/notion" && matches!(&ss.op, SessionStepOp::SendDone { .. })
            } else {
                false
            }
        });
        if saw_pages_in_tool_result || saw_notion_send_done || poll_hit_search {
            break;
        }
        let signature = serde_json::to_string(
            &conversation_items
                .iter()
                .map(|i| (&i.activity_anchor, i.source_name(), &i.content))
                .collect::<Vec<_>>(),
        )
        .expect("serialize conversation snapshot for stagnation poll");
        if signature == last_signature {
            stagnant_polls += 1;
        } else {
            stagnant_polls = 0;
            last_signature = signature;
        }
        if stagnant_polls >= 12 {
            let any_notion_http = mock_hits_poll
                .iter()
                .any(|h| h.starts_with("GET /v1/") || h.starts_with("POST /v1/"));
            if any_notion_http {
                break;
            }
            stagnant_polls = 0;
        }
        sleep(Duration::from_millis(250)).await;
    }

    let mock_hits = mock_state.snapshot().await;
    let saw_mock_search = mock_hits.iter().any(|hit| hit == "POST /v1/search");

    assert!(
        saw_pages_in_tool_result || saw_notion_send_done || saw_mock_search,
        "Expected Notion search: ToolResult.pages, SessionStep SendDone, or mock POST /v1/search. \
         Sources: {:?}, hits: {:?}",
        conversation_items
            .iter()
            .map(|i| i.source_name())
            .collect::<Vec<_>>(),
        mock_hits
    );

    let search_in_provenance = conversation_items.iter().any(|item| match &item.content {
        ConversationItemContent::ToolCall(tc) if tc.tool_name == "support/notion" => true,
        ConversationItemContent::SessionStep(ss) if ss.tool_name == "support/notion" => true,
        _ => false,
    });
    assert!(
        search_in_provenance || saw_mock_search,
        "Expected support/notion in provenance or mock search hit. Sources: {:?}, hits: {:?}",
        conversation_items
            .iter()
            .map(|i| i.source_name())
            .collect::<Vec<_>>(),
        mock_hits
    );

    assert!(
        mock_hits.iter().any(|hit| hit == "POST /v1/search"),
        "Expected mock Notion search endpoint hit. hits={mock_hits:?}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}
