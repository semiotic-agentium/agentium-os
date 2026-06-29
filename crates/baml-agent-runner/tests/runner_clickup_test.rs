// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(feature = "llm-tests", feature = "clickup"))]

mod common;

use std::{fs, path::PathBuf, sync::Arc};

use baml_rt::baml::BamlRuntimeManager;
use baml_rt_conversation::view::{
    ConversationItemContent, ProvenanceConversationContextItem, ToolOutcome,
};
use baml_rt_core::{
    bus::BusWithEffects,
    ids::{AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, PlanningPlanRecord, PlanningPlanStepRecord, ProvEvent, ProvenanceContextReader,
    ProvenancePlanningQuery, ProvenanceWriter, SurrealProvenanceStore,
};
use baml_tools_clickup::ClickUpTool;
use common::{
    RunningHttpServer, TempDirCleanup, build_clickup_agent_to_temp_async, e2e_serial_gate,
    fetch_context_mermaid, post_a2a_sse_collect, start_http_server, start_runner_api_server,
};
use serde_json::{Value, json};
use test_support::common::{
    chunks_from_responses, fnox_has_clickup_key, fnox_has_openrouter_key,
    message_texts_from_chunks, send_stream_request_with_task, test_surreal_store,
    workspace_fnox_path,
};
use tokio::time::{Duration, sleep, timeout};

/// `task-901` description embedded in list/get-task JSON (E2E uses list payload shape).
const FIXTURE_TASK_901_DESCRIPTION: &str =
    "Validate real-model execution against deterministic synthetic tool responses.";

#[derive(Clone, Default)]
struct MockClickUpState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockClickUpState {
    async fn push_hit(&self, entry: String) {
        self.hits.lock().await.push(entry);
    }

    async fn snapshot(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

async fn start_clickup_mock_server() -> std::io::Result<(RunningHttpServer, MockClickUpState)> {
    use axum::{
        Json, Router,
        extract::{Path as AxumPath, State as AxumState},
        routing::get,
    };

    async fn list_teams(AxumState(state): AxumState<MockClickUpState>) -> Json<Value> {
        state.push_hit("GET /api/v2/team".to_string()).await;
        Json(json!({
            "teams": [
                { "id": "9013491519", "name": "Acme Workspace" }
            ]
        }))
    }

    async fn list_spaces(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(team_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .push_hit(format!("GET /api/v2/team/{team_id}/space"))
            .await;
        Json(json!({
            "spaces": [
                { "id": "space-9001", "name": "Engineering" }
            ]
        }))
    }

    async fn list_lists(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(space_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .push_hit(format!("GET /api/v2/space/{space_id}/list"))
            .await;
        Json(json!({
            "lists": [
                { "id": "list-901325431486", "name": "Agent Platform" }
            ]
        }))
    }

    async fn list_tasks(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(list_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .push_hit(format!("GET /api/v2/list/{list_id}/task"))
            .await;
        Json(json!({
            "tasks": [
                {
                    "id": "task-901",
                    "name": "Ship ClickUp integration (E2E)",
                    "status": { "status": "in progress" },
                    "description": FIXTURE_TASK_901_DESCRIPTION,
                    "url": "https://app.clickup.com/t/task-901",
                    "assignees": [{ "username": "qa-bot" }],
                    "priority": { "priority": "high" },
                    "due_date": null
                },
                {
                    "id": "task-902",
                    "name": "Verify Mermaid export endpoint",
                    "status": { "status": "in progress" },
                    "description": "Fetch /contexts/{context_id}/mermaid while runtime is alive and verify sequence output.",
                    "url": "https://app.clickup.com/t/task-902",
                    "assignees": [{ "username": "platform-bot" }],
                    "priority": { "priority": "low" },
                    "due_date": null
                }
            ]
        }))
    }

    async fn get_task(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(task_id): AxumPath<String>,
    ) -> Json<Value> {
        state.push_hit(format!("GET /api/v2/task/{task_id}")).await;
        Json(json!({
            "id": task_id,
            "name": "Ship ClickUp integration (E2E)",
            "status": { "status": "in progress" },
            "description": FIXTURE_TASK_901_DESCRIPTION,
            "url": "https://app.clickup.com/t/task-901",
            "assignees": [{ "username": "qa-bot" }],
            "priority": { "priority": "high" },
            "due_date": null
        }))
    }

    let state = MockClickUpState::default();
    let app = Router::new()
        .route("/api/v2/team", get(list_teams))
        .route("/api/v2/team/{team_id}/space", get(list_spaces))
        .route("/api/v2/space/{space_id}/list", get(list_lists))
        .route("/api/v2/list/{list_id}/task", get(list_tasks))
        .route("/api/v2/task/{task_id}", get(get_task))
        .with_state(state.clone());

    let server = start_http_server(app, Some("/api/v2")).await?;

    Ok((server, state))
}

async fn setup_clickup_agent_with_provenance(
    clickup_api_base_url: &str,
) -> (baml_rt::A2aAgent, Arc<SurrealProvenanceStore>, PathBuf) {
    let built = build_clickup_agent_to_temp_async().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("clickup built path utf8"))
        .expect("load clickup schema");
    manager
        .register_tool(
            ClickUpTool::with_base_url(clickup_api_base_url).expect("construct clickup tool"),
        )
        .await
        .expect("register clickup tool");

    let provenance = test_surreal_store().await;
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("clickup-agent").expect("agent type"),
            "1.0.0".to_string(),
            "clickup-agent@1.0.0".to_string(),
        ))
        .await
        .expect("write AgentBooted");

    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("clickup-agent dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_surreal_store(provenance.clone())
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build clickup agent");
    (agent, provenance, built)
}

fn maybe_task_status(status: &Value) -> Option<String> {
    status.as_str().map(ToOwned::to_owned).or_else(|| {
        status
            .get("status")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

/// ClickUp tool JSON may be stored flat (`tasks`) or wrapped (`result.tasks`) in provenance payloads.
fn clickup_tasks_from_tool_result(value: &Value) -> Option<&Vec<Value>> {
    if let Some(arr) = value.get("tasks").and_then(Value::as_array) {
        return Some(arr);
    }
    value
        .get("result")
        .and_then(|r| r.get("tasks"))
        .and_then(Value::as_array)
}

fn task_priority_as_str(task: &Value) -> Option<&str> {
    task.get("priority").and_then(|p| {
        p.as_str()
            .or_else(|| p.get("priority").and_then(Value::as_str))
    })
}

/// True if assistant text plausibly reports two in-progress tasks (IDs or natural phrasing).
/// The local ClickUp API fixture uses stable task titles — accept those so we do not depend on the model
/// inventing the exact phrase "2 tasks" (capable models still vary wording).
fn assistant_text_reports_two_in_progress_tasks(combined: &str) -> bool {
    if combined.contains("task-901") && combined.contains("task-902") {
        return true;
    }
    let lower = combined.to_lowercase();
    // Titles from the synthetic list payload in this test harness.
    let names_indicate_both_fixture_tasks = lower.contains("ship")
        && lower.contains("clickup")
        && lower.contains("integration")
        && (lower.contains("mermaid") || lower.contains("verify"));
    if lower.contains("in progress") && names_indicate_both_fixture_tasks {
        return true;
    }
    lower.contains("in progress")
        && (lower.contains("2 tasks")
            || lower.contains("two tasks")
            || lower.contains("tasks currently in progress"))
}

/// Returns a JSON object whose top-level `tasks` array has length `n` (walks nested maps).
fn provenance_mentions_clickup_tool(items: &[ProvenanceConversationContextItem]) -> bool {
    items.iter().any(|item| match &item.content {
        ConversationItemContent::ToolCall(tc) => {
            tc.tool_name.contains("clickup") || tc.tool_name == "support/clickup"
        }
        ConversationItemContent::ToolResult(tr) => tr.tool_name.contains("clickup"),
        ConversationItemContent::SessionStep(ss) => ss.tool_name.contains("clickup"),
        ConversationItemContent::Message { .. } => false,
        ConversationItemContent::Operational(_) => false,
        ConversationItemContent::Planning(_) => false,
        ConversationItemContent::CompactionSummary { .. } => false,
    })
}

/// At least one non-message span — tool/session activity (exact sequence is non-deterministic).
fn provenance_has_tooling_activity_shape(items: &[ProvenanceConversationContextItem]) -> bool {
    items.iter().any(|item| {
        matches!(
            &item.content,
            ConversationItemContent::ToolCall(_)
                | ConversationItemContent::ToolResult(_)
                | ConversationItemContent::SessionStep(_)
        )
    })
}

/// Resolve the latest persisted ClickUp plan for observability shape checks.
///
/// Prefer [`ProvenancePlanningQuery::query_current_plan`] (non-superseded head). If that lags or
/// supersession bookkeeping is edge-triggered, fall back to the newest
/// `intent-clickup-*` row from [`ProvenancePlanningQuery::query_plan_history`] (already sorted
/// newest-first). This keeps the E2E stable under async normalizer / multi-turn timing.
async fn resolve_latest_clickup_plan_for_observability(
    store: &SurrealProvenanceStore,
    task_id: &TaskId,
) -> Option<PlanningPlanRecord> {
    for _ in 0..100 {
        if let Ok(Some(p)) = store.query_current_plan(task_id).await {
            return Some(p);
        }
        if let Ok(hist) = store.query_plan_history(task_id, Some(50)).await
            && let Some(p) = hist
                .into_iter()
                .find(|p| p.intent_id.starts_with("intent-clickup-") && !p.steps.is_empty())
        {
            return Some(p);
        }
        sleep(Duration::from_millis(200)).await;
    }
    None
}

/// Persisted A2A plan from the ClickUp agent: linear `step-0…step-(n-1)` and `depends_on` chain.
/// We do **not** assert on step descriptions (LLM prose); only topology + stable id pattern.
/// Plans come from agentic `PlanClickUpWork` only — TypeScript coordinates execution and rejects
/// contract violations instead of injecting fallback steps.
fn assert_clickup_persisted_plan_shape(plan: &PlanningPlanRecord) {
    assert!(
        plan.steps.len() >= 2,
        "ClickUp plan from BAML should surface at least execute + format (>= 2 steps); got {:?}",
        plan.steps
    );
    let mut ordered: Vec<&PlanningPlanStepRecord> = plan.steps.iter().collect();
    ordered.sort_by_key(|s| s.order);
    for (i, step) in ordered.iter().enumerate() {
        let i_u = i as u32;
        assert_eq!(
            step.order, i_u,
            "Planning steps should use contiguous a2a_step_order 0..n-1; steps={:?}",
            plan.steps
        );
        let expected_id = format!("step-{i}");
        assert_eq!(
            step.step_id, expected_id,
            "Planning step_id should match step-{{order}} for this agent's linear plan; steps={:?}",
            plan.steps
        );
        match i {
            0 => assert!(
                step.depends_on.is_empty(),
                "First plan step should have empty depends_on; got {:?}",
                step.depends_on
            ),
            _ => {
                let want_dep = vec![format!("step-{}", i - 1)];
                assert_eq!(
                    step.depends_on, want_dep,
                    "Plan step {} should depend only on the immediate predecessor id; got {:?}",
                    step.step_id, step.depends_on
                );
            }
        }
        assert!(
            !step.status.trim().is_empty(),
            "Plan step {} should record a non-empty status for observability",
            step.step_id
        );
    }
}

fn find_object_with_top_level_tasks_len(value: &Value, n: usize) -> Option<Value> {
    match value {
        Value::Object(m) => {
            if let Some(Value::Array(tasks)) = m.get("tasks")
                && tasks.len() == n
            {
                return Some(Value::Object(m.clone()));
            }
            for v in m.values() {
                if let Some(found) = find_object_with_top_level_tasks_len(v, n) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(a) => {
            for v in a {
                if let Some(found) = find_object_with_top_level_tasks_len(v, n) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

#[tokio::test]
async fn test_e2e_clickup_real_model_with_plan_discovery() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    if !fnox_has_openrouter_key() {
        eprintln!(
            "Skipping test_e2e_clickup_real_model_with_plan_discovery: OPENROUTER_API_KEY not resolved from fnox.toml (source workspace `.env` for env-based secrets)"
        );
        return;
    }
    if !fnox_has_clickup_key() {
        eprintln!(
            "Skipping test_e2e_clickup_real_model_with_plan_discovery: CLICKUP_API_KEY not in env or workspace fnox.toml (CI: Write fnox secrets step; local: fnox default or .env)"
        );
        return;
    }

    let (mock_server, mock_state) = match start_clickup_mock_server().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_real_model_with_mock_server_and_mermaid_http: cannot bind fixture server: {err}"
            );
            return;
        }
    };

    let (agent, provenance_reader, built_dir) =
        setup_clickup_agent_with_provenance(&mock_server.base_url).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);
    let runner_api = match start_runner_api_server(
        "clickup-agent",
        agent,
        provenance_reader.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_real_model_with_mock_server_and_mermaid_http: cannot bind runner API server: {err}"
            );
            return;
        }
    };

    let http_client = reqwest::Client::new();
    let a2a_url = format!(
        "{}/agents/clickup-agent/default/a2a/sse",
        runner_api.base_url
    );
    let context_id = ContextId::new(77, 7);
    let planning_task_id = TaskId::from_external(ExternalId::new("clickup-e2e-plan-obs"));
    let mut matched_tool_result: Option<Value> = None;
    let mut turn_texts: Vec<String> = Vec::new();
    let turn_prompts = [
        "How many tasks are in progress?",
        "Please continue and fetch the required ClickUp data to compute the exact count.",
        "Continue and use tool calls to finish the exact in-progress task count.",
        "If still pending, continue with the next required ClickUp tool call and complete the exact count.",
        "Continue from the same context and finish the exact in-progress count using ClickUp tool calls.",
    ];

    for (turn, prompt) in turn_prompts.iter().enumerate() {
        let correlation_id = baml_rt_core::correlation::generate_correlation_id();
        let request_body = send_stream_request_with_task(
            &format!("clickup-vox-{}", turn + 1),
            prompt,
            correlation_id.as_str(),
            Some(context_id.clone()),
            Some(planning_task_id.clone()),
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
        let texts = message_texts_from_chunks(&chunks);
        turn_texts.extend(texts);

        let mut last_signature = String::new();
        let mut stagnant_polls = 0u32;
        for _ in 0..80 {
            let items = provenance_reader
                .conversation_context(&context_id, Some(220))
                .await
                .unwrap_or_default();
            matched_tool_result = items
                .iter()
                .filter(|item| item.source_name() == "tool_result")
                .find_map(|item| {
                    if let ConversationItemContent::ToolResult(tr) = &item.content
                        && let ToolOutcome::Result(v) = &tr.outcome
                    {
                        if let Some(tasks) = clickup_tasks_from_tool_result(v)
                            && tasks.len() == 2
                        {
                            return Some(v.clone());
                        }
                        if let Some(inner) = find_object_with_top_level_tasks_len(v, 2) {
                            return Some(inner);
                        }
                    }
                    None
                });
            if matched_tool_result.is_some() {
                break;
            }
            let signature = serde_json::to_string(
                &items
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
            if stagnant_polls >= 20 {
                break;
            }
            sleep(Duration::from_millis(250)).await;
        }

        if matched_tool_result.is_some() {
            break;
        }
    }

    // Polling used a bounded window (`Some(220)`); long multi-turn runs truncate early tool rows.
    let conversation_items = provenance_reader
        .conversation_context(&context_id, None)
        .await
        .unwrap_or_default();

    let tool_result = matched_tool_result;
    if let Some(ref tr) = tool_result {
        let tasks = clickup_tasks_from_tool_result(tr).expect("tool_result tasks array");
        assert_eq!(
            tasks.len(),
            2,
            "fixture list payload should contain exactly 2 tasks"
        );
        for task in tasks {
            let status = maybe_task_status(task.get("status").unwrap_or(&Value::Null))
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                status.contains("progress"),
                "Expected in-progress-like status from fixture list payload (shape, not exact phrasing); got {status:?} task={task:?}"
            );

            let description = task
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                !description.trim().is_empty(),
                "Expected non-empty task description, task={task:?}"
            );

            let priority = task_priority_as_str(task)
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                matches!(priority.as_str(), "low" | "high"),
                "Expected task priority low/high, got task={task:?}"
            );
        }
    } else {
        // Session-based ClickUp execution archives list payloads; format-only replans may not
        // re-emit a flat `tool_result` with `tasks`. Local fixture HTTP + assistant grounding still
        // prove the agent consumed the list response.
        let combined = turn_texts.join("\n");
        assert!(
            assistant_text_reports_two_in_progress_tasks(&combined),
            "Expected assistant output to report two in-progress tasks (ids or phrasing). Texts: {turn_texts:?}. \
             Sources seen: {:?}",
            conversation_items
                .iter()
                .map(|i| i.source_name())
                .collect::<Vec<_>>(),
        );
    }

    assert!(
        provenance_mentions_clickup_tool(&conversation_items),
        "Expected provenance to reference the ClickUp tool (tool_call, tool_result, or session_step). \
         Sources seen: {:?}",
        conversation_items
            .iter()
            .map(|i| i.source_name())
            .collect::<Vec<_>>()
    );
    assert!(
        provenance_has_tooling_activity_shape(&conversation_items),
        "Expected at least one tooling span (tool_call / tool_result / session_step); got only messages? \
         sources={:?}",
        conversation_items
            .iter()
            .map(|i| i.source_name())
            .collect::<Vec<_>>()
    );

    // Planning observability: intent + plan must be persisted (submitIntent / submitPlan on execution session).
    let plan = resolve_latest_clickup_plan_for_observability(&provenance_reader, &planning_task_id)
        .await
        .expect(
            "Planning observability: expected a committed ClickUp plan for the stable A2A task_id \
             (query_current_plan and/or query_plan_history with intent-clickup-*). \
             Ensure message.sendStream carries task_id and the agent opens an execution session.",
        );
    assert_clickup_persisted_plan_shape(&plan);
    assert!(
        plan.intent_id.starts_with("intent-clickup-"),
        "Unexpected intent_id in persisted plan: {}",
        plan.intent_id
    );
    assert!(
        plan.plan_id.starts_with("plan-clickup-"),
        "Unexpected plan_id in persisted plan: {}",
        plan.plan_id
    );
    let mut intent = None;
    for _ in 0..80 {
        intent = provenance_reader
            .query_current_intent(&planning_task_id)
            .await
            .ok()
            .flatten();
        if intent.is_some() {
            break;
        }
        if let Ok(hist) = provenance_reader
            .query_intent_history(&planning_task_id, Some(20))
            .await
        {
            intent = hist
                .into_iter()
                .find(|r| r.intent_id.starts_with("intent-clickup-"));
            if intent.is_some() {
                break;
            }
        }
        sleep(Duration::from_millis(200)).await;
    }
    let intent = intent.expect(
        "Planning observability: expected query_current_intent (or history) for the same task_id",
    );
    assert!(
        !intent.description.trim().is_empty(),
        "Persisted intent must carry a non-empty description (shape); exact wording is model-dependent"
    );

    let mock_hits = mock_state.snapshot().await;
    let hit_team = mock_hits.iter().any(|hit| hit == "GET /api/v2/team");
    let hit_list_tasks = mock_hits
        .iter()
        .any(|hit| hit.contains("/list/") && hit.contains("/task"));
    assert!(
        hit_team,
        "Expected ClickUp teams endpoint hit on fixture (workspace entry). hits={mock_hits:?}"
    );
    assert!(
        hit_list_tasks || tool_result.is_some(),
        "Expected list-tasks traffic on fixture or a captured 2-task tool payload (proves enumeration). \
         Model/tool ordering is non-deterministic; we require evidence of task list fetch, not every intermediate hop. hits={mock_hits:?}"
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
    assert!(
        mermaid.contains("ChooseClickUpAction") || mermaid.contains("Choose Click Up Action"),
        "Expected ChooseClickUpAction/Choose Click Up Action (step executor) in context mermaid; got: {mermaid}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}
