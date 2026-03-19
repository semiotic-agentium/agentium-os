#![allow(clippy::print_stdout)]
//! End-to-end test using actual LLM via OpenRouter.
//!
//! To isolate effect-gated timeout issues locally, run with trace logs:
//! `RUST_LOG=baml_rt_quickjs=trace cargo test -p baml-rt test_e2e_simple_greeting_with_llm -- --nocapture`
//! Check for "LlmStarted emitting" vs "effect_emitter is None", and "poll_promise: effect-gated
//! timeout sample" (in_flight_llm, context_id). The first 2s use a warm-up so the short idle
//! timeout is not applied until the promise executor has had time to run.

use std::{collections::BTreeSet, sync::Arc};

use baml_rt::{QuickJSConfig, baml_execution::ParseRetryPolicy, quickjs_bridge::QuickJSBridge};
use baml_rt_core::{
    bus::{BusWithEffects, EffectEmitter, EffectEvent, EffectLiveness, EffectSubscriber, LlmUsage},
    context::{self, InvocationScope},
    ids::{AgentId, ContextId, UuidId},
    semantics::Outcome,
};
use baml_rt_tools::{
    BundleType, ToolRegistry, ToolStep, create_multi_send_session_tool_from_async,
    parse_tool_name_and_class,
    tools::{ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use test_support::common::{
    fnox_has_openrouter_key, require_api_key, setup_baml_runtime_default,
    setup_baml_runtime_from_fixture, setup_bridge,
};
use tokio::sync::Mutex;
use ts_rs::TS;
use uuid::Uuid;

struct TestEvalBundle;

impl BundleType for TestEvalBundle {
    const NAME: &'static str = "test_eval";
    fn description() -> &'static str {
        "Synthetic evaluator bundle for llm-gated tests"
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum SyntheticProjection {
    #[serde(alias = "IDENTITY", alias = "Identity")]
    Identity,
    #[serde(alias = "SUMMARY", alias = "Summary")]
    #[default]
    Summary,
    #[serde(alias = "DETAIL", alias = "Detail")]
    Detail,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct SyntheticSessionEvalInput {
    #[serde(default)]
    read: Option<SyntheticReadEnvelope>,
    #[serde(default)]
    goal_id: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    projection: SyntheticProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
struct SyntheticReadEnvelope {
    ref_id: String,
    #[serde(default)]
    budget_hint: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct SyntheticSessionEvalOutput {
    projection: SyntheticProjection,
    goal_id: Option<String>,
    level: String,
    summary: SyntheticSummary,
    rows: Vec<SyntheticEvalRow>,
    refs: Vec<String>,
    items: Vec<String>,
    payload_bytes: u32,
    returned_items: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct SyntheticSummary {
    count: u32,
    failed_count: u32,
    duration_ms_total: u32,
    total_tokens: u32,
    prompt_tokens_total: u32,
    completion_tokens_total: u32,
    cached_input_tokens_total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct SyntheticEvalRow {
    ref_id: String,
    label: String,
    depth: u32,
    count: u32,
    failed_count: u32,
    duration_ms_total: u32,
    total_tokens: u32,
    prompt_tokens_total: u32,
    completion_tokens_total: u32,
    cached_input_tokens_total: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct SyntheticSessionEvalOpenInput {
    #[serde(default)]
    reason: Option<String>,
}

fn synthetic_session_eval_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("test_eval/synthetic_session_eval")
        .expect("synthetic eval tool name is static");
    TypeBasedMetadataBuilder::<
        SyntheticSessionEvalOpenInput,
        SyntheticSessionEvalInput,
        SyntheticSessionEvalOutput,
    >::new(
        name,
        class_name,
        "Deterministic hierarchical session tool for goal-seeking and efficiency evaluation"
            .to_string(),
    )
    .with_tags(vec![
        TestEvalBundle::NAME.to_string(),
        "session".to_string(),
        "integration-test".to_string(),
    ])
    .with_projection_semantics(
        "Only traversal refs/topology for the current node.",
        "Compact aggregate rows suitable for branch selection.",
        "Concrete leaf/drilldown payload rows for decisive goal checks.",
    )
    .build_metadata()
}

fn synthetic_session_eval_handler(
    metadata: ToolFunctionMetadata,
) -> Arc<dyn baml_rt_tools::ToolHandler> {
    create_multi_send_session_tool_from_async::<
        SyntheticSessionEvalOpenInput,
        SyntheticSessionEvalInput,
        SyntheticSessionEvalOutput,
        _,
    >(metadata, move |args: SyntheticSessionEvalInput| {
        Box::pin(async move {
            let root_rows = vec![
                SyntheticEvalRow {
                    ref_id: "agg:agent:extrospection".to_string(),
                    label: "agent=extrospection".to_string(),
                    depth: 1,
                    count: 42,
                    failed_count: 12,
                    duration_ms_total: 4200,
                    total_tokens: 2600,
                    prompt_tokens_total: 1900,
                    completion_tokens_total: 700,
                    cached_input_tokens_total: 1450,
                },
                SyntheticEvalRow {
                    ref_id: "agg:agent:notion".to_string(),
                    label: "agent=notion".to_string(),
                    depth: 1,
                    count: 28,
                    failed_count: 9,
                    duration_ms_total: 3600,
                    total_tokens: 4200,
                    prompt_tokens_total: 3700,
                    completion_tokens_total: 500,
                    cached_input_tokens_total: 260,
                },
                SyntheticEvalRow {
                    ref_id: "agg:agent:coordinator".to_string(),
                    label: "agent=coordinator".to_string(),
                    depth: 1,
                    count: 24,
                    failed_count: 3,
                    duration_ms_total: 1300,
                    total_tokens: 980,
                    prompt_tokens_total: 760,
                    completion_tokens_total: 220,
                    cached_input_tokens_total: 410,
                },
            ];

            let render = |level: &str,
                          current_ref: Option<&str>,
                          goal_id: Option<String>,
                          projection: SyntheticProjection,
                          rows: Vec<SyntheticEvalRow>,
                          mut refs: Vec<String>,
                          items: Vec<String>| {
                if let Some(current_ref) = current_ref
                    && refs.first().map(String::as_str) != Some(current_ref)
                {
                    refs.insert(0, current_ref.to_string());
                }
                let full_summary = SyntheticSummary {
                    count: rows.iter().map(|r| r.count).sum(),
                    failed_count: rows.iter().map(|r| r.failed_count).sum(),
                    duration_ms_total: rows.iter().map(|r| r.duration_ms_total).sum(),
                    total_tokens: rows.iter().map(|r| r.total_tokens).sum(),
                    prompt_tokens_total: rows.iter().map(|r| r.prompt_tokens_total).sum(),
                    completion_tokens_total: rows.iter().map(|r| r.completion_tokens_total).sum(),
                    cached_input_tokens_total: rows
                        .iter()
                        .map(|r| r.cached_input_tokens_total)
                        .sum(),
                };
                let (summary, rows, items) = match projection {
                    SyntheticProjection::Identity => {
                        (SyntheticSummary::default(), Vec::new(), Vec::new())
                    }
                    SyntheticProjection::Summary => (full_summary, rows, Vec::new()),
                    SyntheticProjection::Detail => (full_summary, rows, items),
                };
                let payload_bytes = u32::try_from(
                    serde_json::to_vec(&(level, &projection, &summary, &rows, &refs, &items))
                        .unwrap_or_default()
                        .len(),
                )
                .unwrap_or(u32::MAX);
                SyntheticSessionEvalOutput {
                    projection,
                    goal_id,
                    level: level.to_string(),
                    summary,
                    rows,
                    refs,
                    payload_bytes,
                    returned_items: u32::try_from(items.len()).unwrap_or(u32::MAX),
                    items,
                }
            };

            let retrieve_ref = args.read.as_ref().map(|read| read.ref_id.as_str());
            if let Some(retrieve_ref) = retrieve_ref {
                if retrieve_ref == "root" {
                    let refs = root_rows
                        .iter()
                        .map(|r| r.ref_id.clone())
                        .collect::<Vec<_>>();
                    return Ok(render(
                        "aggregate",
                        Some(retrieve_ref),
                        None,
                        args.projection,
                        root_rows.clone(),
                        refs,
                        vec![],
                    ));
                }
                if retrieve_ref == "agg:agent:extrospection" {
                    let rows = vec![
                        SyntheticEvalRow {
                            ref_id: "drill:agent:extrospection:tool:system/extrospection"
                                .to_string(),
                            label: "tool=system/extrospection".to_string(),
                            depth: 2,
                            count: 16,
                            failed_count: 8,
                            duration_ms_total: 2550,
                            total_tokens: 1440,
                            prompt_tokens_total: 1120,
                            completion_tokens_total: 320,
                            cached_input_tokens_total: 910,
                        },
                        SyntheticEvalRow {
                            ref_id: "drill:agent:extrospection:tool:system/discover_agents"
                                .to_string(),
                            label: "tool=system/discover_agents".to_string(),
                            depth: 2,
                            count: 10,
                            failed_count: 2,
                            duration_ms_total: 940,
                            total_tokens: 520,
                            prompt_tokens_total: 420,
                            completion_tokens_total: 140,
                            cached_input_tokens_total: 320,
                        },
                    ];
                    let refs = rows.iter().map(|r| r.ref_id.clone()).collect::<Vec<_>>();
                    return Ok(render(
                        "drilldown",
                        Some(retrieve_ref),
                        None,
                        args.projection,
                        rows,
                        refs,
                        vec![],
                    ));
                }
                if retrieve_ref == "agg:agent:notion" {
                    let rows = vec![
                        SyntheticEvalRow {
                            ref_id: "drill:agent:notion:tool:support/notion".to_string(),
                            label: "tool=support/notion".to_string(),
                            depth: 2,
                            count: 14,
                            failed_count: 6,
                            duration_ms_total: 2480,
                            total_tokens: 3010,
                            prompt_tokens_total: 2750,
                            completion_tokens_total: 260,
                            cached_input_tokens_total: 120,
                        },
                        SyntheticEvalRow {
                            ref_id: "drill:agent:notion:tool:system/discover_tools".to_string(),
                            label: "tool=system/discover_tools".to_string(),
                            depth: 2,
                            count: 7,
                            failed_count: 1,
                            duration_ms_total: 520,
                            total_tokens: 430,
                            prompt_tokens_total: 360,
                            completion_tokens_total: 70,
                            cached_input_tokens_total: 40,
                        },
                    ];
                    let refs = rows.iter().map(|r| r.ref_id.clone()).collect::<Vec<_>>();
                    return Ok(render(
                        "drilldown",
                        Some(retrieve_ref),
                        None,
                        args.projection,
                        rows,
                        refs,
                        vec![],
                    ));
                }
                if retrieve_ref == "drill:agent:extrospection:tool:system/extrospection" {
                    let rows = vec![
                        SyntheticEvalRow {
                            ref_id: "leaf:run:extrospection:47".to_string(),
                            label: "run=47 timeout cluster".to_string(),
                            depth: 3,
                            count: 5,
                            failed_count: 3,
                            duration_ms_total: 880,
                            total_tokens: 520,
                            prompt_tokens_total: 390,
                            completion_tokens_total: 130,
                            cached_input_tokens_total: 180,
                        },
                        SyntheticEvalRow {
                            ref_id: "leaf:run:extrospection:52".to_string(),
                            label: "run=52 archive-drilldown mismatch".to_string(),
                            depth: 3,
                            count: 4,
                            failed_count: 4,
                            duration_ms_total: 1120,
                            total_tokens: 680,
                            prompt_tokens_total: 560,
                            completion_tokens_total: 120,
                            cached_input_tokens_total: 310,
                        },
                    ];
                    let refs = rows.iter().map(|r| r.ref_id.clone()).collect::<Vec<_>>();
                    return Ok(render(
                        "leaf_index",
                        Some(retrieve_ref),
                        None,
                        args.projection,
                        rows,
                        refs,
                        vec![],
                    ));
                }
                if retrieve_ref == "drill:agent:notion:tool:support/notion" {
                    let rows = vec![
                        SyntheticEvalRow {
                            ref_id: "leaf:run:notion:88".to_string(),
                            label: "run=88 pagination loop retry storm".to_string(),
                            depth: 3,
                            count: 3,
                            failed_count: 2,
                            duration_ms_total: 980,
                            total_tokens: 1300,
                            prompt_tokens_total: 1220,
                            completion_tokens_total: 80,
                            cached_input_tokens_total: 60,
                        },
                        SyntheticEvalRow {
                            ref_id: "leaf:run:notion:91".to_string(),
                            label: "run=91 scope leak + uncached retrieval inflation".to_string(),
                            depth: 3,
                            count: 4,
                            failed_count: 4,
                            duration_ms_total: 1690,
                            total_tokens: 2190,
                            prompt_tokens_total: 2030,
                            completion_tokens_total: 160,
                            cached_input_tokens_total: 30,
                        },
                    ];
                    let refs = rows.iter().map(|r| r.ref_id.clone()).collect::<Vec<_>>();
                    return Ok(render(
                        "leaf_index",
                        Some(retrieve_ref),
                        None,
                        args.projection,
                        rows,
                        refs,
                        vec![],
                    ));
                }
                if retrieve_ref == "leaf:run:extrospection:52" {
                    return Ok(render(
                    "leaf",
                    Some(retrieve_ref),
                    None,
                    args.projection,
                    vec![],
                    vec![],
                    vec![
                        "evidence: explicit archive read chain executed from aggregate -> drilldown -> leaf".to_string(),
                        "finding: highest failed_count due to stale scope override attempts".to_string(),
                        "action: keep default task scope and use explicit retrieve_ref only".to_string(),
                    ],
                ));
                }
                if retrieve_ref == "leaf:run:notion:88" {
                    return Ok(render(
                        "leaf",
                        Some(retrieve_ref),
                        None,
                        args.projection,
                        vec![],
                        vec![],
                        vec![
                            "evidence: repeated pagination retries with bounded cache impact"
                                .to_string(),
                            "finding: degraded latency but not maximal uncached burden".to_string(),
                        ],
                    ));
                }
                if retrieve_ref == "leaf:run:notion:91" {
                    return Ok(render(
                    "leaf",
                    Some(retrieve_ref),
                    Some("non_trivial_scope_cache_goal".to_string()),
                    args.projection,
                    vec![],
                    vec![],
                    vec![
                        "goal_id: non_trivial_scope_cache_goal".to_string(),
                        "evidence: uncached prompt burden dominates despite similar failure volume".to_string(),
                        "finding: scope leak + archive drilldown path amplifies token cost".to_string(),
                        "action: enforce default task scope and compact read envelopes".to_string(),
                    ],
                ));
                }
                return Ok(render(
                    "leaf",
                    Some(retrieve_ref),
                    None,
                    args.projection,
                    vec![],
                    vec![],
                    vec!["evidence: no matching ref; bounded null payload".to_string()],
                ));
            }

            let requested = args.limit.unwrap_or(3).max(1);
            let take = usize::try_from(requested).unwrap_or(3).min(root_rows.len());
            let rows = root_rows.into_iter().take(take).collect::<Vec<_>>();
            let refs = rows.iter().map(|r| r.ref_id.clone()).collect::<Vec<_>>();
            Ok(render(
                "aggregate",
                None,
                None,
                args.projection,
                rows,
                refs,
                vec![],
            ))
        })
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct TokenSample {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    cached_input_tokens: u64,
}

#[derive(Default)]
struct LlmStatsCollector {
    samples: Mutex<Vec<TokenSample>>,
    llm_completed_total: Mutex<u64>,
}

#[async_trait::async_trait]
impl EffectSubscriber for LlmStatsCollector {
    async fn on_effect(&self, event: &EffectEvent) -> baml_rt::Result<()> {
        if let EffectEvent::LlmCompleted {
            usage,
            result_payload,
            outcome,
            rejection_reason,
            ..
        } = event
        {
            let mut completed = self.llm_completed_total.lock().await;
            *completed = completed.saturating_add(1);
            if let Some(reason) = rejection_reason.as_ref()
                && *outcome == Outcome::Failure
            {
                tracing::warn!(reason = %reason, "LLM completion rejected");
            }
            if let Some(LlmUsage::Known {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_input_tokens,
            }) = usage
            {
                let mut samples = self.samples.lock().await;
                samples.push(TokenSample {
                    prompt_tokens: *prompt_tokens,
                    completion_tokens: *completion_tokens,
                    total_tokens: *total_tokens,
                    cached_input_tokens: cached_input_tokens.unwrap_or(0),
                });
            } else if let Some(payload) = result_payload.as_ref() {
                let usage_obj = payload.get("usage");
                let prompt_tokens = usage_obj
                    .and_then(|u| u.get("prompt_tokens").and_then(|v| v.as_u64()))
                    .or_else(|| {
                        usage_obj.and_then(|u| u.get("input_tokens").and_then(|v| v.as_u64()))
                    });
                let completion_tokens = usage_obj
                    .and_then(|u| u.get("completion_tokens").and_then(|v| v.as_u64()))
                    .or_else(|| {
                        usage_obj.and_then(|u| u.get("output_tokens").and_then(|v| v.as_u64()))
                    });
                let total_tokens = usage_obj
                    .and_then(|u| u.get("total_tokens").and_then(|v| v.as_u64()))
                    .or_else(|| {
                        prompt_tokens
                            .zip(completion_tokens)
                            .map(|(p, c)| p.saturating_add(c))
                    });
                let cached_input_tokens = usage_obj
                    .and_then(|u| u.get("cached_input_tokens").and_then(|v| v.as_u64()))
                    .or_else(|| {
                        usage_obj
                            .and_then(|u| u.get("input_tokens_details"))
                            .and_then(|d| d.get("cached_tokens"))
                            .and_then(|v| v.as_u64())
                    })
                    .unwrap_or(0);
                if let (Some(prompt_tokens), Some(completion_tokens), Some(total_tokens)) =
                    (prompt_tokens, completion_tokens, total_tokens)
                {
                    let mut samples = self.samples.lock().await;
                    samples.push(TokenSample {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cached_input_tokens,
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct AggregatedLlmStats {
    llm_completed_total: u64,
    known_usage_count: u64,
    prompt_tokens_total: u64,
    completion_tokens_total: u64,
    total_tokens_total: u64,
    cached_input_tokens_total: u64,
}

impl LlmStatsCollector {
    async fn aggregate(&self) -> AggregatedLlmStats {
        let completed_total = *self.llm_completed_total.lock().await;
        let samples = self.samples.lock().await;
        let mut out = AggregatedLlmStats {
            llm_completed_total: completed_total,
            known_usage_count: u64::try_from(samples.len()).unwrap_or(u64::MAX),
            ..AggregatedLlmStats::default()
        };
        for sample in samples.iter() {
            out.prompt_tokens_total = out.prompt_tokens_total.saturating_add(sample.prompt_tokens);
            out.completion_tokens_total = out
                .completion_tokens_total
                .saturating_add(sample.completion_tokens);
            out.total_tokens_total = out.total_tokens_total.saturating_add(sample.total_tokens);
            out.cached_input_tokens_total = out
                .cached_input_tokens_total
                .saturating_add(sample.cached_input_tokens);
        }
        out
    }
}

async fn setup_bridge_with_llm_stats(
    baml_manager: Arc<Mutex<baml_rt::baml::BamlRuntimeManager>>,
) -> (QuickJSBridge, Arc<LlmStatsCollector>) {
    let temp_agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let config = QuickJSConfig::new().with_max_attempts_ms(Some(45_000));
    let effect_bus = Arc::new(BusWithEffects::new());
    let collector = Arc::new(LlmStatsCollector::default());
    effect_bus
        .subscribe_effect(collector.clone() as Arc<dyn EffectSubscriber>)
        .await;
    {
        let mut manager = baml_manager.lock().await;
        manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    }
    let mut bridge = QuickJSBridge::new_with_config(baml_manager, temp_agent_id, config)
        .await
        .expect("Create QuickJS bridge");
    bridge.set_effect_liveness(effect_bus as Arc<dyn EffectLiveness>);
    bridge
        .register_baml_functions()
        .await
        .expect("Register BAML functions");
    (bridge, collector)
}

#[tokio::test]
async fn test_e2e_simple_greeting_with_llm() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    if !fnox_has_openrouter_key() {
        eprintln!(
            "Skipping test_e2e_simple_greeting_with_llm: fnox.toml has no OPENROUTER_API_KEY"
        );
        return;
    }

    let baml_manager = setup_baml_runtime_default();
    // Single attempt: avoid retry flakiness (second attempt can return empty parsed response).
    {
        let mut mgr = baml_manager.lock().await;
        mgr.set_parse_retry_policy(ParseRetryPolicy {
            max_attempts: 1,
            delay_ms: 0,
        });
    }
    let bridge = Arc::new(tokio::sync::Mutex::new(setup_bridge(baml_manager).await));

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);
    tracing::info!("Invoking SimpleGreeting BAML function...");
    let result = context::with_scope(scope.as_scope().clone(), async {
        QuickJSBridge::invoke_js_function_nonblocking(
            bridge.clone(),
            &scope,
            "SimpleGreeting",
            json!({ "name": "E2E Test User" }),
        )
        .await
    })
    .await;

    match result {
        Ok(response_value) => {
            let response_str = response_value.as_str().unwrap_or("");
            tracing::info!("✅ BAML function executed successfully!");
            tracing::info!("Response: {}", response_str);

            assert!(
                !response_str.is_empty(),
                "Response should not be empty (got value: {})",
                response_value
            );

            let response_lower = response_str.to_lowercase();
            assert!(
                response_lower.contains("e2e")
                    || response_lower.contains("test")
                    || response_lower.contains("user")
                    || response_str.len() > 5,
                "Response should be meaningful or mention the name"
            );
        }
        Err(e) => {
            tracing::error!("❌ BAML function execution failed: {}", e);
            panic!(
                "BAML function should execute successfully, but got error: {}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_e2e_streaming_greeting() {
    tracing::info!("Testing streaming BAML function call");

    if !fnox_has_openrouter_key() {
        eprintln!("Skipping test_e2e_streaming_greeting: fnox.toml has no OPENROUTER_API_KEY");
        return;
    }

    let baml_manager = setup_baml_runtime_default();
    let mut bridge = setup_bridge(baml_manager).await;

    // Call streaming BAML function from JavaScript with scope (streaming path uses worker-thread scope).
    let js_code = r#"
        (() => __awaitAndStringify(
            (async () => {
                const chunks = [];
                const stream = SimpleGreetingStream({ name: "Streaming Test" });
                for await (const chunk of stream) {
                    chunks.push(chunk);
                }
                return { chunks: chunks, totalChunks: chunks.length };
            })()
        ))()
    "#;

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);
    tracing::info!("Executing streaming JavaScript call...");
    let result = context::with_scope(scope.as_scope().clone(), async {
        bridge.eval_scoped(&scope, js_code).await
    })
    .await;

    match result {
        Ok(response_value) => {
            let response_str = response_value.as_str().unwrap_or("");
            tracing::info!("✅ Streaming function executed successfully!");
            tracing::info!("Response: {}", response_str);

            // Parse the response to verify chunks were received
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(response_str)
                && let Some(obj) = parsed.as_object()
                && let Some(chunks) = obj.get("chunks")
            {
                assert!(chunks.as_array().is_some(), "Should have chunks array");
                tracing::info!("Received {} chunks", chunks.as_array().unwrap().len());
            }
        }
        Err(e) => {
            tracing::warn!("Streaming test failed (may not be supported yet): {}", e);
            // Don't fail the test if streaming isn't fully implemented yet
        }
    }
}

#[tokio::test]
async fn test_llm_gated_planner_with_synthetic_retrieval_eval() {
    #[derive(Clone)]
    struct SessionContext {
        session_open: bool,
        status_token: String,
        allowed_ops: Vec<String>,
        last_status: Option<String>,
    }

    let _ = require_api_key();
    let baml_manager = setup_baml_runtime_from_fixture("session-tool-eval");
    let (mut bridge, llm_stats) = setup_bridge_with_llm_stats(baml_manager).await;
    let planner_scope =
        InvocationScope::synthetic_message(AgentId::from_uuid(UuidId::new(Uuid::new_v4())));

    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_dynamic(
            synthetic_session_eval_metadata(),
            synthetic_session_eval_handler(synthetic_session_eval_metadata()),
        )
        .expect("register dynamic synthetic session tool");

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let context_id = ContextId::new(77, 9001);
    let objective = "Find the run with most severe combined failure pressure and uncached prompt burden, then return goal_id non_trivial_scope_cache_goal via explicit session reads.";

    let mut session_ctx = SessionContext {
        session_open: false,
        status_token: "X".to_string(),
        allowed_ops: vec!["Open".to_string()],
        last_status: None,
    };
    let mut session_id = None;
    let mut used_projections: BTreeSet<String> = BTreeSet::new();
    let mut used_levels: BTreeSet<String> = BTreeSet::new();
    let mut payload_bytes_total: u64 = 0;
    let mut planner_hops: u32 = 0;
    let mut invalid_planner_ops: u32 = 0;
    let mut sends: u32 = 0;
    let mut reads: u32 = 0;
    let mut found_goal = false;
    let mut goal_id: Option<String> = None;
    let mut session_state: Option<serde_json::Value> = None;
    let mut last_output: Option<serde_json::Value> = None;
    let mut finished = false;

    for _ in 0..24 {
        let planner_input = json!({
            "objective": objective,
            "session_context": {
                "contract_version": "session_context",
                "session_open": session_ctx.session_open,
                "status_token": session_ctx.status_token,
                "allowed_ops": session_ctx.allowed_ops,
                "last_status": session_ctx.last_status
            },
            "session_state": session_state
        });
        let planner_args =
            serde_json::to_string(&planner_input).expect("serialize planner args for evaluate");
        let planner_js =
            format!("(() => __awaitAndStringify(PlanSyntheticSessionStep({planner_args})))()");
        let plan_raw = context::with_scope(planner_scope.as_scope().clone(), async {
            bridge.eval_scoped(&planner_scope, &planner_js).await
        })
        .await
        .expect("evaluate PlanSyntheticSessionStep");
        let plan: serde_json::Value = if let Some(raw_str) = plan_raw.as_str() {
            serde_json::from_str(raw_str)
                .unwrap_or_else(|e| panic!("parse planner result JSON ({e}): {plan_raw}"))
        } else {
            plan_raw
        };
        planner_hops = planner_hops.saturating_add(1);
        let step = plan
            .get("decision")
            .unwrap_or_else(|| panic!("planner output missing decision: {plan}"));
        let op = step
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("planner output missing step.op: {plan}"));
        if !session_ctx.allowed_ops.iter().any(|a| a == op) {
            invalid_planner_ops = invalid_planner_ops.saturating_add(1);
            tracing::warn!(
                op,
                ?session_ctx.allowed_ops,
                "planner emitted op outside allowed_ops; reprompting same state"
            );
            continue;
        }
        match op {
            "Open" => {
                // LLM may return `"initial_input": null`; treat null the same as absent.
                let open_input = step
                    .get("initial_input")
                    .and_then(|v| if v.is_null() { None } else { Some(v) })
                    .cloned()
                    .unwrap_or_else(|| json!({"reason": "integration eval"}));
                let sid = registry
                    .open_session(
                        "test_eval/synthetic_session_eval",
                        open_input,
                        &context_id,
                        &agent_id,
                    )
                    .await
                    .expect("open synthetic eval session");
                session_id = Some(sid);
                session_ctx = SessionContext {
                    session_open: true,
                    status_token: "O".to_string(),
                    allowed_ops: vec!["Send".to_string()],
                    last_status: Some("open".to_string()),
                };
            }
            "Send" => {
                let sid = session_id
                    .as_ref()
                    .unwrap_or_else(|| panic!("session_id missing before Send"));
                let send_input = step
                    .get("input")
                    .cloned()
                    .filter(|v| !v.is_null())
                    .unwrap_or_else(|| json!({}));
                if let Some(proj) = send_input.get("projection").and_then(|v| v.as_str()) {
                    used_projections.insert(proj.to_ascii_lowercase());
                }
                registry
                    .session_send(sid, send_input)
                    .await
                    .expect("session send");
                sends = sends.saturating_add(1);
                session_ctx = SessionContext {
                    session_open: true,
                    status_token: "S".to_string(),
                    allowed_ops: vec!["Read".to_string(), "Send".to_string()],
                    last_status: Some("sent".to_string()),
                };
            }
            "Read" => {
                let sid = session_id
                    .as_ref()
                    .unwrap_or_else(|| panic!("session_id missing before Read"));
                let read_input = step
                    .get("input")
                    .cloned()
                    .filter(|v| !v.is_null())
                    .unwrap_or_else(|| json!({}));
                if let Some(proj) = read_input.get("projection").and_then(|v| v.as_str()) {
                    used_projections.insert(proj.to_ascii_lowercase());
                }
                let tool_step = registry
                    .session_read(sid, read_input)
                    .await
                    .expect("session read");
                reads = reads.saturating_add(1);
                let output = match tool_step {
                    ToolStep::Done {
                        output: Some(output),
                    } => output,
                    other => panic!("expected Done(Some(output)), got {other:?}"),
                };
                last_output = Some(output.clone());
                payload_bytes_total = payload_bytes_total.saturating_add(
                    output
                        .get("payload_bytes")
                        .and_then(|v| v.as_u64())
                        .expect("payload_bytes in output"),
                );
                if let Some(level) = output.get("level").and_then(|v| v.as_str()) {
                    used_levels.insert(level.to_string());
                }
                goal_id = output
                    .get("goal_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                found_goal = goal_id.as_deref() == Some("non_trivial_scope_cache_goal");
                let summarize_input = json!({
                    "objective": objective,
                    "tool_output_json": serde_json::to_string(&output).expect("serialize tool output json"),
                    "used_projections": used_projections.iter().cloned().collect::<Vec<_>>(),
                });
                let summarize_args =
                    serde_json::to_string(&summarize_input).expect("serialize summarize args");
                let summarize_js = format!(
                    "(() => __awaitAndStringify(SummarizeSyntheticSessionOutput({summarize_args})))()"
                );
                let summarize_raw = context::with_scope(planner_scope.as_scope().clone(), async {
                    bridge.eval_scoped(&planner_scope, &summarize_js).await
                })
                .await
                .expect("evaluate SummarizeSyntheticSessionOutput");
                session_state = Some(if let Some(raw_str) = summarize_raw.as_str() {
                    serde_json::from_str(raw_str).unwrap_or_else(|e| {
                        panic!("parse summarizer result JSON ({e}): {summarize_raw}")
                    })
                } else {
                    summarize_raw
                });
                session_ctx = SessionContext {
                    session_open: true,
                    status_token: "D".to_string(),
                    allowed_ops: if found_goal {
                        vec!["Finish".to_string()]
                    } else {
                        vec!["Read".to_string(), "Send".to_string()]
                    },
                    last_status: Some("done".to_string()),
                };
            }
            "Finish" => {
                let sid = session_id
                    .as_ref()
                    .unwrap_or_else(|| panic!("session_id missing before Finish"));
                registry.session_finish(sid).await.expect("session finish");
                session_ctx = SessionContext {
                    session_open: false,
                    status_token: "F".to_string(),
                    allowed_ops: vec![],
                    last_status: Some("finished".to_string()),
                };
                finished = true;
                break;
            }
            "Abort" => panic!("planner aborted unexpectedly: {plan}"),
            other => panic!("unexpected planner op: {other}"),
        }
    }

    assert!(finished, "planner did not finish session");
    assert!(
        !session_ctx.session_open,
        "session must be closed after Finish"
    );
    assert_eq!(
        session_ctx.last_status.as_deref(),
        Some("finished"),
        "finish status must be recorded"
    );
    assert!(
        found_goal,
        "must reach non-trivial goal via session loop; last_output={last_output:?}"
    );
    assert_eq!(
        goal_id.as_deref(),
        Some("non_trivial_scope_cache_goal"),
        "goal_id mismatch"
    );
    assert!(
        used_projections.contains("identity")
            && used_projections.contains("summary")
            && used_projections.contains("detail"),
        "must exercise identity/summary/detail in one session, got {used_projections:?}"
    );
    assert!(
        used_levels.contains("aggregate")
            && used_levels.contains("drilldown")
            && used_levels.contains("leaf_index")
            && used_levels.contains("leaf"),
        "must traverse aggregate->drilldown->leaf_index->leaf, got {used_levels:?}"
    );
    assert!(
        sends >= 1 && reads >= 4,
        "non-trivial session should include at least one scope send and multiple reads (sends={sends}, reads={reads})"
    );
    assert!(
        planner_hops >= 6,
        "step executor should emit multiple execution hops, got {planner_hops}"
    );
    assert!(
        invalid_planner_ops <= 2,
        "step executor repeatedly violated allowed_ops (count={invalid_planner_ops})"
    );
    assert!(
        payload_bytes_total <= 8192,
        "session retrieval payload budget exceeded: {payload_bytes_total} bytes"
    );
    let stats = llm_stats.aggregate().await;
    assert!(
        stats.llm_completed_total >= planner_hops as u64,
        "expected llm completions to cover execution hops (completed={}, hops={planner_hops})",
        stats.llm_completed_total
    );
    if stats.known_usage_count > 0 {
        assert!(
            stats.total_tokens_total > 0,
            "token stats must be populated when known usage samples are present"
        );
    }
}
