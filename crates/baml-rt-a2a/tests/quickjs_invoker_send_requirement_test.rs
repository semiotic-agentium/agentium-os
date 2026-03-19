//! **Purpose:** Enforce that the real QuickJS stream handover remains live under
//! concurrent collection, including INPUT_REQUIRED turn boundaries.

use std::sync::Arc;

use baml_rt::{QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{A2aRequestHandler, collect_a2a_stream};
use test_support::common::{chunks_from_responses, message_texts_from_chunks, send_stream_request};
use tokio::time::{Duration, timeout};

fn init_trace() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

fn response_has_final_chunk(response: &serde_json::Value) -> bool {
    response
        .get("result")
        .and_then(|res| res.get("final"))
        .and_then(|f| f.as_bool())
        == Some(true)
}

fn chunk_state(chunk: &serde_json::Value) -> Option<&str> {
    chunk
        .get("task")
        .and_then(|task| task.get("status"))
        .and_then(|status| status.get("state"))
        .and_then(|state| state.as_str())
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|status_update| status_update.get("status"))
                .and_then(|status| status.get("state"))
                .and_then(|state| state.as_str())
        })
}

enum CompletionKind {
    Final,
    InputRequired,
}

struct StreamCase {
    tag: &'static str,
    request_text: &'static str,
    expect: CompletionKind,
    min_chunks: usize,
}

async fn collect_with_agent(
    agent: baml_rt_a2a::A2aAgent,
    request: serde_json::Value,
) -> Vec<serde_json::Value> {
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
        .await
        .expect("handle_a2a_stream");
    collect_a2a_stream(stream)
        .await
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_stream_phase_matrix_regression_under_load() {
    init_trace();

    let agent = baml_rt_a2a::A2aAgent::builder()
        .with_runtime_manager(BamlRuntimeManager::builder().build().unwrap())
        .with_init_js(
        r#"
        globalThis.onChatMessage = async function(message) {
            const text = (message && message.parts && message.parts[0] && message.parts[0].text) || "unknown";
            const match = /^([^:]+):(\d+)(?::(.*))?$/.exec(text);
            const tag = match ? match[1] : "unknown";
            const chunks = match ? parseInt(match[2] || "6", 10) : 6;
            const mode = match ? (match[3] || "final") : "final";

            for (let i = 0; i < chunks; i++) {
                if (i % 2 === 0) {
                    __chat_yield({ message: { parts: [{ text: `${tag}-${i}` }] } });
                } else {
                    __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
                }
            }

            if (mode === "input") {
                __chat_yield({ task: { status: { state: "TASK_STATE_INPUT_REQUIRED" } } });
            }
            __chat_yield({ final: true });
        };
    "#,
        )
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(
            QuickJSConfig::new()
                .with_max_attempts_ms(Some(15_000))
                .with_stream_concurrency(Some(12))
                .with_stream_collector_idle_secs(Some(5)),
        )
        .build()
        .await
        .unwrap();

    let cases = [
        StreamCase {
            tag: "alpha",
            request_text: "alpha:16:final",
            expect: CompletionKind::Final,
            min_chunks: 1,
        },
        StreamCase {
            tag: "beta",
            request_text: "beta:12:input",
            expect: CompletionKind::InputRequired,
            min_chunks: 1,
        },
        StreamCase {
            tag: "gamma",
            request_text: "gamma:8:final",
            expect: CompletionKind::Final,
            min_chunks: 1,
        },
        StreamCase {
            tag: "delta",
            request_text: "delta:6:final",
            expect: CompletionKind::Final,
            min_chunks: 1,
        },
    ];

    for wave_start in (0..cases.len()).step_by(2) {
        let mut joins = Vec::new();
        for (index_in_wave, case) in cases[wave_start..].iter().take(2).enumerate() {
            let wave_index = wave_start / 2 + 1;
            let request_id = wave_index * 10 + index_in_wave + 1;
            let request = send_stream_request(
                &format!("msg-{}-w{}", case.tag, wave_index),
                case.request_text,
                &format!("corr-1700000000010-{}", request_id),
                None,
            );
            joins.push((
                case,
                tokio::spawn(collect_with_agent(agent.clone(), request)),
            ));
        }

        let mut outputs = Vec::new();
        for (case, join) in joins {
            let responses = timeout(Duration::from_secs(12), join)
                .await
                .expect("each stream in wave must finish")
                .expect("collect task must not panic");
            outputs.push((case, responses));
        }

        for (case, responses) in outputs {
            assert!(
                !responses.is_empty(),
                "stream {} should emit some response",
                case.tag
            );

            let chunks = chunks_from_responses(&responses);
            assert!(
                chunks.len() >= case.min_chunks,
                "stream {} should emit at least {} chunks (got {})",
                case.tag,
                case.min_chunks,
                chunks.len()
            );

            let has_final = responses.iter().any(response_has_final_chunk);
            let has_input_required = chunks
                .iter()
                .any(|chunk| chunk_state(chunk) == Some("TASK_STATE_INPUT_REQUIRED"));

            match case.expect {
                CompletionKind::Final => {
                    assert!(
                        has_final,
                        "stream {} should complete with final marker",
                        case.tag
                    );
                }
                CompletionKind::InputRequired => {
                    // INPUT_REQUIRED is a turn boundary, not a terminal final chunk.
                    assert!(!has_final, "stream {} should not emit final", case.tag);
                    assert!(
                        has_input_required,
                        "stream {} should emit INPUT_REQUIRED",
                        case.tag
                    );
                }
            }

            let has_tag = message_texts_from_chunks(&chunks)
                .iter()
                .any(|text| text.contains(case.tag));
            assert!(
                has_tag,
                "stream {} should include its tag in yielded payload",
                case.tag
            );
        }
    }
}
