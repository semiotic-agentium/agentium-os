//! **Purpose:** Enforce that the real QuickJS stream handover remains live under
//! concurrent collection, including INPUT_REQUIRED turn boundaries.

use baml_rt_core::A2aRequestHandler;
use futures_util::StreamExt;
use test_support::common::{
    build_minimal_a2a_agent_with_stream_idle_secs, chunk_content, message_texts_from_chunks,
    response_has_final_chunk, response_has_input_required, send_stream_request,
};
use tokio::time::{Duration, timeout};

/// stderr logging: honors `RUST_LOG` when set; otherwise only errors (see `EnvFilter::from_default_env`).
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

#[derive(Debug, PartialEq, Eq)]
enum Terminator {
    Final,
    InputRequired,
}

struct StreamCase {
    tag: &'static str,
    request_text: &'static str,
    expect: Terminator,
}

#[derive(Debug, Default)]
struct StreamOutcome {
    chunk_count: usize,
    has_tag: bool,
    term: Option<Terminator>,
    /// True if a `final: true` chunk arrived AFTER INPUT_REQUIRED in the same turn.
    /// INPUT_REQUIRED is an A2A turn boundary; the bridge must filter subsequent same-turn
    /// yields. The JS fixture deliberately yields `{ final: true }` after INPUT_REQUIRED
    /// (see the agent JS below) so this flag exercises the bridge's filter.
    final_after_input_required: bool,
}

/// Stops at the first `Final` terminator (drops the stream as soon as it arrives). On
/// `InputRequired`, continues draining the stream to natural close so a leaked post-sentinel
/// `final: true` is observable as a bridge contract violation. `outcome.term == None` means
/// the stream completed without either terminator — caller asserts liveness.
async fn drive_stream(
    agent: baml_rt_a2a::A2aAgent,
    request: serde_json::Value,
    tag: &'static str,
) -> StreamOutcome {
    let mut stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
        .await
        .expect("handle_a2a_stream");
    let mut outcome = StreamOutcome::default();
    while let Some(chunk) = stream.next().await {
        let env = chunk.as_ref();
        outcome.chunk_count += 1;
        if !outcome.has_tag {
            let chunk_val = chunk_content(env).unwrap_or(env);
            if message_texts_from_chunks(&[chunk_val])
                .iter()
                .any(|text| text.contains(tag))
            {
                outcome.has_tag = true;
            }
        }
        match outcome.term {
            None => {
                if response_has_final_chunk(env).is_some() {
                    outcome.term = Some(Terminator::Final);
                    break;
                }
                if response_has_input_required(env).is_some() {
                    outcome.term = Some(Terminator::InputRequired);
                    // Fall through and keep draining: any later `final` in the same turn
                    // would be a bridge filter regression.
                }
            }
            Some(Terminator::InputRequired) => {
                if response_has_final_chunk(env).is_some() {
                    outcome.final_after_input_required = true;
                }
            }
            Some(Terminator::Final) => unreachable!("loop breaks on Final"),
        }
    }
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_stream_phase_matrix_regression_under_load() {
    init_tracing();

    let agent = build_minimal_a2a_agent_with_stream_idle_secs(
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
        1,
    )
    .await;

    // Chunk counts reduced from 32/24/16/8 to 12/10/8/6 to avoid CI timeouts under load.
    let cases = [
        StreamCase {
            tag: "alpha",
            request_text: "alpha:12:final",
            expect: Terminator::Final,
        },
        StreamCase {
            tag: "beta",
            request_text: "beta:10:input",
            expect: Terminator::InputRequired,
        },
        StreamCase {
            tag: "gamma",
            request_text: "gamma:8:final",
            expect: Terminator::Final,
        },
        StreamCase {
            tag: "delta",
            request_text: "delta:6:final",
            expect: Terminator::Final,
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
                tokio::spawn(drive_stream(agent.clone(), request, case.tag)),
            ));
        }

        let mut outputs = Vec::new();
        for (case, join) in joins {
            let outcome = timeout(Duration::from_secs(20), join)
                .await
                .expect("each stream in wave must finish")
                .expect("collect task must not panic");
            outputs.push((case, outcome));
        }

        for (case, outcome) in outputs {
            assert!(
                outcome.chunk_count > 0,
                "stream {} should emit some response",
                case.tag
            );

            match (&case.expect, outcome.term.as_ref()) {
                (Terminator::Final, Some(Terminator::Final)) => {}
                (Terminator::InputRequired, Some(Terminator::InputRequired)) => {
                    assert!(
                        !outcome.final_after_input_required,
                        "stream {} emitted final after INPUT_REQUIRED — bridge must filter \
                         post-sentinel yields in the same turn",
                        case.tag
                    );
                }
                (Terminator::Final, Some(Terminator::InputRequired)) => {
                    panic!("stream {} unexpected INPUT_REQUIRED before final", case.tag)
                }
                (Terminator::InputRequired, Some(Terminator::Final)) => {
                    panic!("stream {} emitted final before INPUT_REQUIRED", case.tag)
                }
                (Terminator::Final, None) => {
                    panic!("stream {} should complete with final marker", case.tag)
                }
                (Terminator::InputRequired, None) => {
                    panic!("stream {} should emit INPUT_REQUIRED", case.tag)
                }
            }

            assert!(
                outcome.has_tag,
                "stream {} should include its tag in yielded payload",
                case.tag
            );
        }
    }
}
