// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Verifies the per-hop merged projected history lines (same merge as BAML
//! `ctx.tags['conversation_transcript']`) seen at each `intercept_llm_call` in a
//! `run_step_executor_loop` (graph provider + loop-local supplement, mirrored for the interceptor).
//!
//! Merged lines must follow BAML transcript rules: no session FSM bookkeeping (`Open` /
//! `SendDone`), session Open tool-call `describe_open` duplicates, or opcode-only assistant text.
//!
//! To update: `INSTA_UPDATE=1 cargo test -p baml-rt-quickjs --test step_executor_intra_turn_conversation_history_snapshot`
#![recursion_limit = "256"]

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use async_trait::async_trait;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    bus::{BusWithEffects, EffectEmitter},
    context::{self, InvocationScope},
    ids::{AgentId, UuidId},
};
use baml_rt_interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor};
use baml_rt_provenance::{ProvEvent, ProvenanceWriter, SurrealStoreBuilder};
use baml_rt_quickjs::step_executor_loop::run_step_executor_loop;
use serde_json::{Value, json};
use tokio::sync::RwLock;

fn redact_stochastic_strings(v: &Value) -> Value {
    use std::sync::OnceLock;

    use regex::Regex;
    static UUID: OnceLock<Regex> = OnceLock::new();
    static HASH_REF: OnceLock<Regex> = OnceLock::new();
    static AT_REF: OnceLock<Regex> = OnceLock::new();
    let uuid = UUID.get_or_init(|| {
        Regex::new(r#"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"#)
            .expect("uuid re")
    });
    let hash_ref = HASH_REF.get_or_init(|| Regex::new(r"#\d+").expect("hash ref re"));
    let at_ref = AT_REF.get_or_init(|| Regex::new(r"@\d+").expect("at ref re"));
    fn visit(v: &Value, uuid: &Regex, hash_ref: &Regex, at_ref: &Regex) -> Value {
        match v {
            Value::String(s) => {
                let t = uuid.replace_all(s, "<redacted-uuid>");
                let t = hash_ref.replace_all(&t, "<redacted-#ref>");
                let t = at_ref.replace_all(&t, "<redacted-@ref>");
                Value::String(t.to_string())
            }
            Value::Array(a) => {
                Value::Array(a.iter().map(|x| visit(x, uuid, hash_ref, at_ref)).collect())
            }
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (k.clone(), visit(x, uuid, hash_ref, at_ref)))
                    .collect(),
            ),
            o => o.clone(),
        }
    }
    visit(v, uuid, hash_ref, at_ref)
}

const FSM_OPCODE_LABELS: &[&str] = &["Open", "Send", "Finish", "Abort", "SearchRead", "PageRead"];

/// Strip `#N` / redacted ref prefix so opcode guards match projected content.
fn content_after_history_ref(content: &str) -> &str {
    let trimmed = content.trim();
    if let Some(rest) = trimmed.strip_prefix("<redacted-#ref>") {
        return rest.trim();
    }
    if let Some(rest) = trimmed.strip_prefix('#') {
        let digit_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digit_len > 0 {
            return rest[digit_len..].trim_start();
        }
    }
    trimmed
}

fn assert_transcript_rows_spec_clean(rows: &[Value]) {
    let mut tool_contents: Vec<String> = Vec::new();
    for row in rows {
        let content = row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let core = content_after_history_ref(content);
        assert!(
            !FSM_OPCODE_LABELS.contains(&core),
            "FSM opcode must not appear in merged transcript row: {row:?}"
        );
        if row.get("role").and_then(Value::as_str) == Some("tool") {
            tool_contents.push(core.to_string());
        }
    }
    for (i, a) in tool_contents.iter().enumerate() {
        for b in tool_contents.iter().skip(i + 1) {
            assert!(
                a != b,
                "duplicate identical tool transcript rows are forbidden: {a:?}"
            );
        }
    }
}

struct HistorySnapshotInterceptor {
    call_count: Arc<AtomicU32>,
    manager: Arc<RwLock<BamlRuntimeManager>>,
    /// Current loop supplement for this hop (mirrored from `run_step_executor_loop` before `invoke`)
    step_intra_slice: Arc<std::sync::Mutex<Vec<Value>>>,
    /// `merged_conversation_history_lines_json` for each `intercept_llm_call` in order
    per_llm: Arc<std::sync::Mutex<Vec<Value>>>,
}

#[async_trait]
impl LLMInterceptor for HistorySnapshotInterceptor {
    async fn intercept_llm_call(
        &self,
        ctx: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        // Same merged lines as the current `invoke_function_with_intra` formats into BAML
        // `ctx.tags['conversation_transcript']` (before the LLM; supplement from the loop).
        let lines = {
            let supplement: Vec<Value> = self
                .step_intra_slice
                .lock()
                .expect("step_intra slice mutex poisoned")
                .clone();
            let g = self.manager.read().await;
            g.merged_conversation_history_lines_json(&ctx.runtime_scope, &supplement)
                .await
        }?;
        self.per_llm
            .lock()
            .expect("interceptor per_llm mutex poisoned")
            .push(lines);

        // Same response sequence as `test_step_executor_loop_drives_full_session_with_interceptor`
        let n = self.call_count.fetch_add(1, Ordering::Relaxed);
        let response = match n {
            0 => json!({ "step": { "op": "Open" } }),
            1 => json!({
                "step": {
                    "op": "Send",
                    "input": { "expression": { "left": 2, "operation": "Add", "right": 3 } }
                }
            }),
            2 => json!({ "step": { "op": "Finish" } }),
            _ => json!({ "message": "done" }),
        };
        Ok(InterceptorDecision::Substitute(response))
    }

    async fn on_llm_call_complete(
        &self,
        _ctx: &LLMCallContext,
        _result: &baml_rt_core::Result<Value>,
        _ms: u64,
    ) {
    }
}

#[tokio::test]
async fn step_executor_intra_turn_merged_conversation_grows_per_llm_hop() {
    test_support::common::ensure_fixture_runtime_types();
    let agent_dir =
        test_support::common::workspace_root().join("tests/fixtures/agents/stream-baml-tool");
    let built =
        test_support::common::build_agent_package_to_temp(agent_dir, "stream-baml-tool").await;

    // Task-scoped + graph seed so the provenance conversation query can grow per hop; see
    // `test_step_executor_loop_drives_full_session_with_interceptor` in bridge_test.rs.
    let scope = InvocationScope::synthetic_task(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
    ));
    let (context_id, message_id, task_id) = {
        let rs = scope.as_scope();
        let task_id = rs
            .task_id_opt()
            .expect("synthetic_task has task_id")
            .clone();
        (rs.context_id().clone(), rs.message_id().clone(), task_id)
    };

    let mut m = BamlRuntimeManager::builder().build().unwrap();
    m.load_schema(built.to_str().unwrap()).expect("load schema");
    let manifest = baml_rt_tools::ManifestToolNames::parse(&["support/calculate".to_string()])
        .expect("parse manifest");
    let policy = baml_rt_tools::parse_access_allowlist();
    m.set_tool_allowlist(
        ["support/calculate"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    )
    .await
    .unwrap();
    baml_rt_tools::register_manifest_tools(m.tool_registry().as_ref(), &manifest, &policy).unwrap();
    m.rebuild_function_tool_manifest();

    let bus: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory provenance for step executor snapshot");
    m.set_effect_emitter(bus.clone());
    let manager = Arc::new(RwLock::new(m));
    baml_rt::a2a_transport::install_provenance_conversation_wiring(
        store.clone(),
        Some(store.clone()),
        &manager,
        &bus,
        Arc::new(baml_rt_provenance::FixedCompactionSummarizer::new(
            "Prior conversation was compacted; continue from recent context.",
        )),
        &baml_rt_llm_config::LlmClientConfig::sensible_default(),
        None,
    )
    .await
    .expect("provenance + conversation wiring");

    {
        let agent = scope.as_scope().agent_id().clone();
        store
            .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
            .await
            .expect("task exists");
        store
            .add_event(ProvEvent::task_execution_started(
                context_id.clone(),
                task_id.clone(),
                agent.clone(),
            ))
            .await
            .expect("task execution started");
        store
            .add_event(ProvEvent::message_received_task(
                context_id.clone(),
                task_id,
                message_id,
                "user".to_string(),
                vec!["intra turn merged snapshot".to_string()],
                None,
                agent,
                1_700_000_010_404,
            ))
            .await
            .expect("message received");
    }

    let call_count = Arc::new(AtomicU32::new(0));
    let per_llm: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let step_intra_mirror: Arc<std::sync::Mutex<Vec<Value>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    {
        let i = HistorySnapshotInterceptor {
            call_count: call_count.clone(),
            manager: manager.clone(),
            step_intra_slice: step_intra_mirror.clone(),
            per_llm: per_llm.clone(),
        };
        let w = manager.write().await;
        w.register_llm_interceptor(i).await;
    }

    let out = context::with_scope(
        scope.as_scope().clone(),
        run_step_executor_loop(
            &manager,
            scope.as_scope(),
            "ChooseCalcTool",
            json!({ "user_message": "what is 2 + 3?" }),
            8,
            Some(step_intra_mirror),
        ),
    )
    .await
    .expect("step executor loop should complete");
    assert!(out.steps.len() >= 3, "expected >=3 FSM hops");

    let per_llm: Vec<Value> = per_llm
        .lock()
        .expect("unpoisoned")
        .iter()
        .cloned()
        .collect();
    // Non-decreasing per hop (some FSM hops may not append transcript rows yet); overall the graph
    // still gains rows across the full executor run.
    let as_arrays: Vec<Vec<Value>> = per_llm
        .iter()
        .map(|v| v.as_array().cloned().unwrap_or_default().to_vec())
        .collect();
    for (hop, rows) in as_arrays.iter().enumerate() {
        assert_transcript_rows_spec_clean(rows);
        assert!(
            !rows.is_empty(),
            "hop {hop} merged transcript must not be empty"
        );
    }
    let lengths: Vec<usize> = as_arrays.iter().map(|a| a.len()).collect();
    for w in lengths.windows(2) {
        assert!(
            w[1] >= w[0],
            "merged conversation must not shrink between hops: {:?}",
            lengths
        );
    }
    assert!(
        lengths.first().copied().unwrap_or(0) < lengths.last().copied().unwrap_or(0),
        "merged conversation should grow across the run: {:?}",
        lengths
    );

    let report =
        json!({ "per_llm_merged_conversation_history": per_llm, "step_count": out.steps.len() });
    insta::assert_json_snapshot!(&redact_stochastic_strings(&report));
}
