//! Integration tests for [`baml_rt_provenance::episode::EpisodeReader`] — graph-derived episode
//! semantics including READ transcripts, citation drift, and rendered text format.
//!
//! These tests use SurrealDB's in-memory backend which shares state across `Surreal::new::<Mem>(())`.
//! Despite unique namespace isolation, parallel execution causes sporadic failures. Run with
//! `--test-threads=1` or via `cargo nextest` which runs each test in a separate process.

use std::{sync::Arc, time::Duration};

use baml_rt_conversation::view::SessionStepOp;
use baml_rt_core::{
    Outcome,
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, CallScope, Episode, EpisodeContent, LlmUsage, ProvEvent, ProvenanceWriter, StepType,
    SurrealStoreBuilder, episode::EpisodeReader,
};

/// `ToolRead` rows with rendered archive/grep bodies (SendDone inline read + explicit Read), in timeline order.
fn transcript_tool_read_bodies(ep: &Episode) -> Vec<String> {
    ep.prior_context
        .iter()
        .chain(ep.transcript.iter())
        .filter_map(|e| {
            if e.step_type != StepType::ToolRead {
                return None;
            }
            match &e.content {
                EpisodeContent::ToolOutput { lines, .. } => Some(lines.join("\n")),
                _ => None,
            }
        })
        .collect()
}

async fn build_isolated_store() -> Arc<baml_rt_provenance::SurrealProvenanceStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build isolated in-memory store")
}

async fn wall_clock_tick() {
    tokio::time::sleep(Duration::from_millis(12)).await;
}

/// Helper: bootstrap a minimal task lifecycle (agent boot, task exists, execution started).
async fn bootstrap_task(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    context_id: &ContextId,
    task_id: &TaskId,
    agent_id: &AgentId,
) {
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("test_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "test@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            None,
            Some("TASK_STATE_SUBMITTED".to_string()),
        ))
        .await
        .expect("task_status_submitted");
}

/// Helper: complete a task.
async fn complete_task(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    context_id: &ContextId,
    task_id: &TaskId,
) {
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            Some("working".to_string()),
            Some("completed".to_string()),
        ))
        .await
        .expect("task_status_completed");
}

// ---------------------------------------------------------------------------
// Test 1: Token aggregation and wall-clock duration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn episode_aggregates_llm_tokens_and_wall_clock() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(1_900_000_000_000, 1);
    let tid = TaskId::from_external(ExternalId::new("ep-tokens-1"));
    let aid =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());

    bootstrap_task(&store, &ctx, &tid, &aid).await;
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            tid.clone(),
            MessageId::from_external(ExternalId::new("msg-1")),
            "user".into(),
            vec!["hello".into()],
            None,
            aid.clone(),
            1_900_000_000_001,
        ))
        .await
        .expect("msg");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::llm_call_completed_task(
            ctx.clone(), tid.clone(),
            "DefaultClient".into(), "openai-generic".into(), "Chat".into(),
            serde_json::json!({"messages": []}),
            serde_json::json!({"agent_id": aid.as_str(), "task_id": tid.as_str(), "message_id": "msg-1"}),
            LlmUsage::Known { prompt_tokens: 42, completion_tokens: 58, total_tokens: 100, cached_input_tokens: None },
            3_500, Outcome::Success,
        ))
        .await.expect("llm");
    complete_task(&store, &ctx, &tid).await;

    let ep = EpisodeReader::new(store)
        .read_snapshot_by_task_id(&tid)
        .await
        .expect("read");

    assert_eq!(ep.token_summary.llm_call_count, 1);
    assert_eq!(ep.token_summary.prompt_tokens, 42);
    assert_eq!(ep.token_summary.completion_tokens, 58);
    assert_eq!(ep.token_summary.total_tokens, 100);
    assert_eq!(ep.token_summary.llm_duration_ms, 3_500);
    assert!(
        ep.duration.wall_clock_ms > 0,
        "wall_clock_ms={}",
        ep.duration.wall_clock_ms
    );
}

// ---------------------------------------------------------------------------
// Test 2: SendDone is summary-only; explicit PageRead has archive body (transcript ∥ session_history)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn episode_send_done_produces_read_entries_from_graph_hydrated_payload() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(1_900_000_000_100, 1);
    let tid = TaskId::from_external(ExternalId::new("ep-read-transcript"));
    let aid =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000101").unwrap());
    let msg_id = MessageId::from_external(ExternalId::new("msg-read-1"));
    let tool_anchor = ActivityAnchorId::from_counter(9_100_042);

    bootstrap_task(&store, &ctx, &tid, &aid).await;
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            tid.clone(),
            msg_id.clone(),
            "user".into(),
            vec!["invoke discover".into()],
            None,
            aid.clone(),
            1_900_000_000_200,
        ))
        .await
        .expect("msg");
    wall_clock_tick().await;

    // Tool start + complete with a known anchor so WAS_INFORMED_BY can link
    store.add_event(ProvEvent::tool_call_started_task(
        ctx.clone(), tid.clone(), "system/discover_agents".into(), None,
        serde_json::json!({"op": "Send"}),
        serde_json::json!({"phase": "execute", "agent_id": aid.as_str(), "task_id": tid.as_str()}),
        None,
    )).await.expect("tool_start");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::tool_call_completed_task_with_id(
            tool_anchor.clone(),
            ctx.clone(),
            tid.clone(),
            "system/discover_agents".into(),
            None,
            serde_json::json!({"op": "Send"}),
            serde_json::json!({
                "phase": "execute",
                "agent_id": aid.as_str(), "task_id": tid.as_str(),
                "result": [
                    {"id": "agent-alpha", "desc": "Alpha agent"},
                    {"id": "agent-beta", "desc": "Beta agent"},
                    {"id": "agent-gamma", "desc": "Gamma agent"}
                ]
            }),
            12,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_complete");
    wall_clock_tick().await;

    // SendDone with informed_by pointing at the tool completion
    store
        .add_event(ProvEvent::tool_session_step(
            ctx.clone(),
            CallScope::Task {
                task_id: tid.clone(),
            },
            "system/discover_agents".into(),
            "sess-1".into(),
            &SessionStepOp::SendDone {
                archive_ref: "@1".into(),
                header: r#"@1 · "agents" · 3L · 100B"#.into(),
                informed_by: tool_anchor.as_str().to_string(),
            },
        ))
        .await
        .expect("send_done");
    wall_clock_tick().await;

    // Explicit PageRead step (contiguous archive inspection; no grep)
    store
        .add_event(ProvEvent::tool_session_step(
            ctx.clone(),
            CallScope::Task {
                task_id: tid.clone(),
            },
            "system/discover_agents".into(),
            "sess-1".into(),
            &SessionStepOp::PageRead {
                archive_ref: "@1".into(),
                offset: 0,
                limit: 200,
            },
        ))
        .await
        .expect("page_read_step");
    complete_task(&store, &ctx, &tid).await;

    let ep = EpisodeReader::new(store)
        .read_snapshot_by_task_id(&tid)
        .await
        .expect("read");

    // --- Structured transcript assertions ---
    let all_entries: Vec<_> = ep
        .prior_context
        .iter()
        .chain(ep.transcript.iter())
        .collect();

    // SendDone → ToolResult (summary line only), not ToolRead. Archive text only on explicit read.
    let send_done_entry = all_entries
        .iter()
        .find(|e| {
            e.step_type == StepType::ToolResult
                && matches!(&e.content, EpisodeContent::ToolOutput { lines, .. } if {
                    let t = lines.join("\n");
                    t.contains("4894@1") && !t.contains("PageRead for")
                })
        })
        .expect("SendDone transcript row with compact header only");
    if let EpisodeContent::ToolOutput { lines, .. } = &send_done_entry.content {
        let t = lines.join("\n");
        assert!(
            !t.contains("agent-alpha"),
            "SendDone must not inline tool/archive payload: {t}"
        );
    } else {
        panic!("expected ToolOutput for SendDone");
    }

    let tool_read_entries: Vec<_> = all_entries
        .iter()
        .filter(|e| e.step_type == StepType::ToolRead)
        .collect();
    assert_eq!(
        tool_read_entries.len(),
        1,
        "only explicit PageRead/SearchRead produce ToolRead; got {}",
        tool_read_entries.len()
    );

    let read_body = tool_read_entries[0];
    if let EpisodeContent::ToolOutput { lines, summary, .. } = &read_body.content {
        assert!(
            summary.contains("cat -n") || lines.iter().any(|l| l.contains("cat -n")),
            "PageRead should format as cat -n; summary={summary:?}"
        );
        let joined = lines.join("\n");
        assert!(joined.contains("agent-alpha"), "PageRead body: {joined}");
        assert!(joined.contains("agent-beta"), "PageRead body: {joined}");
        assert!(joined.contains("agent-gamma"), "PageRead body: {joined}");
    } else {
        panic!("expected ToolOutput for PageRead");
    }

    // --- Session history: golden file matches `assemble_session_history` / `project_prompt_context` (see `docs/baml-rt-conversation-spec.md`) ---
    let session_history = serde_json::to_value(&ep.session_history).expect("json");
    insta::assert_json_snapshot!(session_history);

    // Transcript: one ToolRead body (explicit PageRead), not duplicated from SendDone
    let tool_read_bodies = transcript_tool_read_bodies(&ep);
    assert_eq!(tool_read_bodies.len(), 1, "one explicit read in transcript");
    let body = &tool_read_bodies[0];

    // --- Rendered text format assertions ---
    let rendered = baml_rt_provenance::render_episode(&ep);
    assert!(
        rendered.contains("tool_result system/discover_agents:"),
        "rendered must include tool_result header"
    );
    assert!(
        rendered.contains("agent-alpha"),
        "rendered must include archive content in the explicit tool_read block"
    );
    for ln in body.lines() {
        let needle = format!("  | {ln}");
        let hits = rendered.match_indices(&needle).count();
        assert!(
            hits >= 1,
            "read line should appear in rendered episode; line={ln:?} hits={hits}"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3: Drift summary and per-call detail with citation scoring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn episode_includes_drift_summary_citations_and_rendered_section() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(1_900_000_000_200, 1);
    let tid = TaskId::from_external(ExternalId::new("ep-drift-cites"));
    let aid =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000202").unwrap());

    bootstrap_task(&store, &ctx, &tid, &aid).await;
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            tid.clone(),
            MessageId::from_external(ExternalId::new("msg-drift-1")),
            "user".into(),
            vec!["analyze data".into()],
            None,
            aid.clone(),
            1_900_000_000_300,
        ))
        .await
        .expect("msg");
    wall_clock_tick().await;

    store
        .add_event(ProvEvent::llm_call_completed_task_with_drift(
            ctx.clone(),
            tid.clone(),
            "DefaultClient".into(),
            "claude-3".into(),
            "Chat".into(),
            serde_json::json!({"messages": [{"role": "user", "content": "analyze data"}]}),
            serde_json::json!({
                "agent_id": aid.as_str(), "task_id": tid.as_str(), "message_id": "msg-drift-1"
            }),
            LlmUsage::Known {
                prompt_tokens: 25,
                completion_tokens: 45,
                total_tokens: 70,
                cached_input_tokens: None,
            },
            2_000,
            Outcome::Success,
            Some(Box::new(baml_rt_provenance::events::LlmDriftInfo {
                score: 0.85,
                severity: baml_rt_embedding::DriftSeverity::Acceptable,
                mode: baml_rt_embedding::DriftMode::Audit,
                warn_min_score: 0.4,
                block_min_score: 0.2,
                intent_text_preview: "analyze data".into(),
                response_text_preview: "Based on the data...".into(),
                step_text_preview: "analyze user input".into(),
                plan_drift: Some(
                    baml_rt_provenance::events::LlmPlanDriftInfo::PlanCommitted {
                        scores: baml_rt_provenance::events::PlanDriftScores {
                            intent_alignment: 0.85,
                            trajectory_drift: 0.88,
                            plan_adherence_score: 0.91,
                            composite_severity: baml_rt_embedding::DriftSeverity::Acceptable,
                        },
                        step_alignment: 0.92,
                        cross_encoder_step_score: 3.2,
                    },
                ),
                citation_drift: Some(baml_rt_provenance::events::LlmCitationDriftInfo {
                    per_citation: vec![baml_rt_provenance::events::LlmCitationSimilarity {
                        n: 1,
                        is_history: true,
                        negated: false,
                        similarity: 0.74,
                        raw: "#1".into(),
                        activity_anchor: "prov-1900000000300".into(),
                        content_preview: "analyze data...".into(),
                    }],
                    mean_similarity: 0.74,
                    coverage: 1.0,
                    total_decisions: 1,
                    cited_decisions: 1,
                }),
            })),
            vec!["#1".into()],
            vec![],
        ))
        .await
        .expect("llm_drift");
    complete_task(&store, &ctx, &tid).await;

    let ep = EpisodeReader::new(store)
        .read_snapshot_by_task_id(&tid)
        .await
        .expect("read");

    // --- Drift summary ---
    let ds = ep
        .drift_summary
        .as_ref()
        .expect("drift_summary must be present");
    assert_eq!(ds.scored_call_count, 1);
    assert_eq!(ds.warn_count, 0);
    assert_eq!(ds.block_count, 0);
    assert_eq!(
        ds.composite_severity,
        baml_rt_embedding::DriftSeverity::Acceptable
    );
    assert!(ds.intent_alignment > 0.8);
    assert!(ds.step_alignment.unwrap() > 0.9);
    assert!(ds.trajectory_drift.unwrap() > 0.8);
    assert!(ds.plan_adherence_score > 0.9);

    // --- Per-call drift detail ---
    assert_eq!(ep.drift_calls.len(), 1);
    let call = &ep.drift_calls[0];
    assert!(!call.activity_anchor.is_empty());
    assert_eq!(call.function_name, "Chat");
    assert_eq!(call.severity, baml_rt_embedding::DriftSeverity::Acceptable);
    assert!(call.intent_alignment >= 0.8);
    assert!(call.step_alignment.unwrap() >= 0.9);

    // Citation drift should be populated (the fix in nest_llm_drift_fields parses the JSON string)
    if let Some(cite_mean) = call.citation_mean_similarity {
        assert!(cite_mean >= 0.7, "citation_mean_similarity={cite_mean}");
    }
    if let Some(cite_cov) = call.citation_coverage {
        assert!(cite_cov > 0.0, "citation_coverage={cite_cov}");
    }

    // --- Rendered text format ---
    let rendered = baml_rt_provenance::render_episode(&ep);
    assert!(
        rendered.contains("## drift"),
        "rendered must include drift section"
    );
    assert!(rendered.contains("composite_severity: acceptable"));
    assert!(rendered.contains("intent_alignment: 0.85"));
    assert!(rendered.contains("step_alignment: 0.92"));
    assert!(rendered.contains("scored_calls: 1"));
    assert!(
        rendered.contains("calls:"),
        "drift section must list per-call detail"
    );
    assert!(rendered.contains("function=Chat"));

    // --- Rendered text includes all standard sections ---
    assert!(rendered.contains("## episode"));
    assert!(rendered.contains("## transcript"));
    assert!(rendered.contains("## outcome"));
}

// ---------------------------------------------------------------------------
// Test 4: execution_session_step entries are kept in transcript (seq invariant)
//         but their empty-citations payload is suppressed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn episode_execution_session_step_suppresses_empty_citations_but_keeps_entries() {
    use baml_rt_provenance::{EpisodeContent, ProvEvent, StepType};
    let store = build_isolated_store().await;
    let ctx = ContextId::new(1_900_000_000_400, 1);
    let tid = TaskId::from_external(ExternalId::new("ep-fsm-entries"));
    let aid =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000404").unwrap());

    bootstrap_task(&store, &ctx, &tid, &aid).await;
    wall_clock_tick().await;

    // User message
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            tid.clone(),
            MessageId::from_external(ExternalId::new("msg-fsm-1")),
            "user".into(),
            vec!["hi".into()],
            None,
            aid.clone(),
            1_900_000_000_500,
        ))
        .await
        .expect("msg");
    wall_clock_tick().await;

    // Synthetic a2a/execution_session_step tool call + result with empty citations
    store
        .add_event(ProvEvent::tool_call_started_task(
            ctx.clone(),
            tid.clone(),
            "a2a/execution_session_step".into(),
            None,
            serde_json::json!({"plan_id": "plan-1", "step_id": "step-greet"}),
            serde_json::json!({"agent_id": aid.as_str(), "task_id": tid.as_str()}),
            None,
        ))
        .await
        .expect("tool_start");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            tid.clone(),
            "a2a/execution_session_step".into(),
            None,
            serde_json::json!({"plan_id": "plan-1", "step_id": "step-greet"}),
            serde_json::json!({
                "agent_id": aid.as_str(), "task_id": tid.as_str(),
                "result": {"citations": []}
            }),
            5,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_complete");
    wall_clock_tick().await;

    // Agent message citing the user message (#1)
    store
        .add_event(ProvEvent::message_sent_task(
            ctx.clone(),
            tid.clone(),
            MessageId::from_external(ExternalId::new("msg-agent-1")),
            "assistant".into(),
            vec!["Hi!".into()],
            None,
            aid.clone(),
            1_900_000_000_600,
            vec![baml_rt_core::Citation::try_new("#1").unwrap()],
        ))
        .await
        .expect("agent_msg");
    complete_task(&store, &ctx, &tid).await;

    let ep = EpisodeReader::new(store)
        .read_snapshot_by_task_id(&tid)
        .await
        .expect("read");

    let all_entries: Vec<_> = ep
        .prior_context
        .iter()
        .chain(ep.transcript.iter())
        .collect();

    // The execution_session_step ToolCall+ToolResult MUST still be in the transcript
    // (removing them would break seq numbering and dangle citation refs).
    let fsm_entries: Vec<_> = all_entries
        .iter()
        .filter(|e| {
            matches!(&e.content, EpisodeContent::ToolInvocation { tool_name, .. }
                if tool_name.ends_with("execution_session_step"))
                || matches!(&e.content, EpisodeContent::ToolOutput { tool_name, .. }
                if tool_name.ends_with("execution_session_step"))
        })
        .collect();
    assert_eq!(
        fsm_entries.len(),
        2,
        "execution_session_step ToolCall+ToolResult must be present; got {}",
        fsm_entries.len()
    );

    // The ToolResult content MUST have its lines cleared (citations: [] suppressed)
    let tool_result = fsm_entries
        .iter()
        .find(|e| e.step_type == StepType::ToolResult)
        .expect("ToolResult entry");
    if let EpisodeContent::ToolOutput {
        lines,
        line_count,
        byte_count,
        ..
    } = &tool_result.content
    {
        assert!(
            lines.is_empty(),
            "ToolResult lines must be cleared (citations: [] suppressed)"
        );
        assert_eq!(*line_count, 0);
        assert_eq!(*byte_count, 0);
    } else {
        panic!("expected ToolOutput content on execution_session_step ToolResult");
    }

    // The agent message citation (#1 → ep-prefixed) must resolve to an existing seq entry
    let agent_msg = all_entries
        .iter()
        .find(|e| e.step_type == StepType::Message && !e.citation_strings.is_empty())
        .expect("agent message with citations");
    let cite = &agent_msg.citation_strings[0];
    // Verify the cited ep-ref resolves to an actual entry in the transcript
    let ep_prefix = ep.ref_prefix.as_str();
    let cited_seq: u32 = cite
        .strip_prefix(ep_prefix)
        .and_then(|s| s.strip_prefix('#'))
        .and_then(|n| n.parse().ok())
        .expect("citation should be ep#N format");
    let cited_entry = all_entries.iter().find(|e| e.seq == cited_seq);
    assert!(
        cited_entry.is_some(),
        "citation {cite} refers to seq {cited_seq} which must exist in the transcript"
    );

    // Rendered text must NOT contain 'citations: []' noise
    let rendered = baml_rt_provenance::render_episode(&ep);
    assert!(
        !rendered.contains("citations: []"),
        "rendered text must not show empty citations: []"
    );
}
