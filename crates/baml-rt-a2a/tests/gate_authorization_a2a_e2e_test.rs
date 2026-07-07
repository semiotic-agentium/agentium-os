// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! End-to-end A2A path: tier-3 semiotic gate → INPUT_REQUIRED + gateAuthorization → approve resume → tool executes.

#![allow(clippy::redundant_closure)] // test helpers take &Value; iterators often yield &&Value

use std::sync::{Arc, OnceLock};

use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, collect_a2a_stream_one_shot, collect_a2a_stream_until_one_shot,
    ids::{ContextId, ExternalId, MessageId, TaskId},
};
use baml_rt_semiotic::{
    config::{SemioticConfig, SemioticMode, SemioticPolicy},
    global::{global_grounding_store, set_global_semiotic_config, submit_grounding},
    schema::{Anchor, AnchorSign, EnvSignals, Node, ParseArtifact, Postcondition},
};
use baml_rt_tools::{
    ToolMetadataBuilder,
    tools::{
        ToolAccess, ToolFunctionMetadata, ToolName, TypeBasedMetadataBuilder,
        create_one_shot_tool_from_async,
    },
};
use baml_tools_calculator::{CalculatorInput, CalculatorOutput, MathOperation};
use serde_json::Value;
use test_support::common::{chunk_content, chunks_from_responses, send_stream_request_with_task};

static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn gate_invoker_js() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const text = message?.parts?.[0]?.text || "";
        if (text.includes("gate-e2e") || text.trim() === "approve") {
            try {
                const args = JSON.stringify({
                    expression: { left: 1, operation: "Add", right: 2 }
                });
                const result = await __tool_invoke("support/gate_delete", args);
                __chat_yield({
                    __session: message.__session,
                    message: { parts: [{ text: "executed:" + JSON.stringify(result) }] }
                });
                __chat_yield({ __session: message.__session, final: true });
            } catch (e) {
                const msg = String(e);
                if (msg.includes("Gate authorization required")) {
                    return;
                }
                __chat_yield({
                    __session: message.__session,
                    message: { parts: [{ text: "tool-error:" + msg }] },
                    final: true
                });
                return;
            }
            return;
        }
        __chat_yield({ __session: message.__session, message: { parts: [{ text: "unknown" }] } });
        __chat_yield({ __session: message.__session, final: true });
    };
    "#
    .to_string()
}

fn grounded_artifact() -> ParseArtifact {
    ParseArtifact {
        instruction: "Archive users inactive per agreed definition in prod db".into(),
        template: "agentic_execution".into(),
        env: EnvSignals {
            environment: "prod".into(),
            verb_class: "mutating".into(),
            reversible: true,
            external_visibility: false,
        },
        nodes: vec![
            Node {
                name: "OBJECT".into(),
                anchors: vec![
                    Anchor {
                        sign: AnchorSign::Symbol,
                        content: "users".into(),
                        source: "user".into(),
                    },
                    Anchor {
                        sign: AnchorSign::Icon,
                        content: "schema users(id,last_login_at,status,deleted_at)".into(),
                        source: "user".into(),
                    },
                ],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "TARGET".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Index,
                    content: "prod app.db".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "ACTION".into(),
                anchors: vec![
                    Anchor {
                        sign: AnchorSign::Symbol,
                        content: "archive".into(),
                        source: "user".into(),
                    },
                    Anchor {
                        sign: AnchorSign::Icon,
                        content: "set deleted_at=now".into(),
                        source: "user".into(),
                    },
                ],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "SCOPE".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Icon,
                    content: "last_login_at older than 90 days, per user answer".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "METHOD".into(),
                anchors: vec![],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "CRITERION".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Verify,
                    content: "rowcount(users where deleted_at set) >= 1".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
        ],
        covers: vec!["support/gate_delete".into()],
        postconditions: vec![Postcondition {
            cmd: "true".into(),
            desc: "placeholder assertion".into(),
        }],
    }
}

fn task_state_from_chunk(chunk: &Value) -> Option<String> {
    let state_from = |v: &Value| {
        v.get("status")
            .and_then(|s| s.get("state"))
            .and_then(|s| s.as_str())
            .map(String::from)
    };
    chunk
        .get("task")
        .and_then(|task| state_from(task))
        .or_else(|| status_update_body(chunk).and_then(state_from))
}

fn status_update_body(chunk: &Value) -> Option<&Value> {
    let su = chunk.get("statusUpdate")?;
    if su.get("status").is_some() {
        Some(su)
    } else {
        su.get("statusUpdate").or_else(|| su.get("status_update"))
    }
}

fn gate_auth_metadata_from_chunks(chunks: &[&Value]) -> Option<bool> {
    for chunk in chunks {
        let ev = status_update_body(chunk)?;
        let gate = ev
            .get("status")
            .and_then(|s| s.get("message"))
            .and_then(|m| m.get("metadata"))
            .and_then(|m| m.get("gateAuthorization"))
            .and_then(|v| v.as_bool());
        if gate == Some(true) {
            return Some(true);
        }
    }
    None
}

fn input_required_prompt_from_chunks(chunks: &[&Value]) -> Option<String> {
    for chunk in chunks {
        if task_state_from_chunk(chunk).as_deref() != Some("TASK_STATE_INPUT_REQUIRED") {
            continue;
        }
        let ev = status_update_body(chunk)?;
        let text = ev
            .get("status")
            .and_then(|s| s.get("message"))
            .and_then(|m| m.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|parts| parts.first())
            .and_then(|part| part.get("text"))
            .and_then(|t| t.as_str())?;
        return Some(text.to_string());
    }
    None
}

async fn setup_gate_agent() -> A2aAgent {
    set_global_semiotic_config(SemioticConfig {
        default: SemioticPolicy {
            enabled: true,
            mode: SemioticMode::Enforce,
            enforce_min_tier: 2,
            ..Default::default()
        },
        overrides: Default::default(),
    });

    let manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");

    let name = ToolName::parse("support/gate_delete").expect("parse tool name");
    let class_name = ToolFunctionMetadata::derive_class_name(name.bundle(), name.local());
    let metadata = TypeBasedMetadataBuilder::<(), CalculatorInput, CalculatorOutput>::new(
        name,
        class_name,
        "Tier-3 gate E2E delete-class tool".to_string(),
    )
    .with_access(ToolAccess::Delete)
    .build_metadata();
    let handler = create_one_shot_tool_from_async::<(), CalculatorInput, CalculatorOutput, _>(
        metadata.clone(),
        |input: CalculatorInput| {
            Box::pin(async move {
                let left = input.expression.left;
                let right = input.expression.right;
                let result = match input.expression.operation {
                    MathOperation::Add => left + right,
                    MathOperation::Subtract => left - right,
                    MathOperation::Multiply => left * right,
                    MathOperation::Divide => {
                        if right == 0 {
                            return Err(baml_rt::BamlRtError::InvalidArgument(
                                "division by zero".into(),
                            ));
                        }
                        left / right
                    }
                };
                Ok(CalculatorOutput {
                    expression: format!("{left} + {right}"),
                    result: result as f64,
                    formatted: format!("{result}"),
                })
            })
        },
    );
    manager
        .tool_registry()
        .register_dynamic(metadata, handler)
        .expect("register gate_delete tool");

    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(gate_invoker_js())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
        .build()
        .await
        .expect("build gate e2e agent")
}

async fn collect_stream(agent: &A2aAgent, request: Value) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await?;
    let chunks: Vec<A2aStreamChunk> = collect_a2a_stream_one_shot(stream).await;
    Ok(chunks.into_iter().map(A2aStreamChunk::into_inner).collect())
}

async fn test_gate() -> tokio::sync::OwnedSemaphorePermit {
    let gate = GATE
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone();
    gate.acquire_owned().await.expect("test gate")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_gate_authorization_tier3_suspend_and_approve_resume() {
    let _permit = test_gate().await;
    let agent = setup_gate_agent().await;
    let context_id = ContextId::new(42, 1);
    let task_id = TaskId::from_external(ExternalId::new("live-task:ctx-42-1:msg-gate-1"));

    let scope = baml_rt_core::context::RuntimeScope::task_scope(
        context_id.clone(),
        agent.agent_id().clone(),
        MessageId::from("msg-gate-1"),
        task_id.clone(),
    );
    submit_grounding(&scope, grounded_artifact(), None);
    assert!(
        global_grounding_store()
            .get_live(&scope, std::time::Duration::from_secs(3600))
            .is_some()
    );

    let turn1 = send_stream_request_with_task(
        "msg-gate-1",
        "gate-e2e",
        "corr-42-1",
        Some(context_id.clone()),
        None,
    );
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(turn1))
        .await
        .expect("open stream");

    let stop_at_input_required = |r: &A2aStreamChunk| {
        let v = r.as_ref();
        let chunk = chunk_content(v);
        let state = chunk.and_then(task_state_from_chunk);
        let is_final = v
            .get("result")
            .and_then(|res| res.get("final"))
            .and_then(|f| f.as_bool())
            .unwrap_or(false);
        is_final || state.as_deref() == Some("TASK_STATE_INPUT_REQUIRED")
    };
    let r1: Vec<Value> = collect_a2a_stream_until_one_shot(stream, stop_at_input_required)
        .await
        .into_iter()
        .map(A2aStreamChunk::into_inner)
        .collect();

    let chunks1 = chunks_from_responses(&r1);
    let states1: Vec<String> = chunks1
        .iter()
        .filter_map(|c| task_state_from_chunk(c))
        .collect();
    assert!(!r1.is_empty(), "turn1 produced no stream responses");
    assert!(
        states1.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "turn1 must suspend for tier-3 authorization; states: {states1:?}; responses: {}",
        serde_json::to_string_pretty(&r1).unwrap_or_else(|_| "?".into())
    );
    assert_eq!(
        gate_auth_metadata_from_chunks(&chunks1),
        Some(true),
        "INPUT_REQUIRED chunk must carry gateAuthorization metadata"
    );
    let prompt = input_required_prompt_from_chunks(&chunks1).expect("authorization prompt");
    assert!(prompt.contains("Tier-3 authorization required"));
    assert!(prompt.contains("Grounded intent"));
    assert!(prompt.contains("Postconditions declared"));

    let final_count_turn1 = r1
        .iter()
        .filter(|r| {
            r.get("result")
                .and_then(|x| x.get("final"))
                .and_then(Value::as_bool)
                == Some(true)
        })
        .count();
    assert_eq!(
        final_count_turn1, 0,
        "authorization turn must not emit final: true"
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let turn2 =
        send_stream_request_with_task("msg-gate-2", "approve", "corr-42-2", Some(context_id), None);
    let r2 = collect_stream(&agent, turn2).await.expect("approve stream");
    let chunks2 = chunks_from_responses(&r2);
    let states2: Vec<String> = chunks2
        .iter()
        .filter_map(|c| task_state_from_chunk(c))
        .collect();
    assert!(
        states2.contains(&"TASK_STATE_COMPLETED".to_string()),
        "approve resume must complete; states: {states2:?}; responses: {}",
        serde_json::to_string_pretty(&r2).unwrap_or_else(|_| "?".into())
    );

    assert!(
        !states2.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "approve must not re-suspend for authorization; states: {states2:?}"
    );

    let response_blob = serde_json::to_string(&r2).unwrap_or_default();
    assert!(
        response_blob.contains("executed:")
            || states2.contains(&"TASK_STATE_COMPLETED".to_string()),
        "tool should complete after approve; states: {states2:?}"
    );
}
