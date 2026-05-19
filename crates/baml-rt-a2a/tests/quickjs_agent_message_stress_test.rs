//! Stress the **QuickJS + A2A** path: many concurrent `message.sendStream` calls on **one** agent
//! (one [`BridgeHandle`] / handover lane → `invoke_js_function_stream` → `onChatMessage`).
//!
//! **Hotspot (measured):** streams are **serialized** on that lane. Concurrent fan-in queues work;
//! wall time grows **superlinearly** with `N` (e.g. `N=16`, `yields=8` → ~50s wall on a typical laptop).
//! JSON-RPC `id` must be `corr-<millis>-<counter>` when set; otherwise [`A2aRequestHandler::handle_a2a_stream`]
//! returns [`baml_rt_core::BamlRtError::InvalidArgument`] (fail-fast, not an empty stream).
//!
//! Run:
//! ```text
//! QUICKJS_STRESS_N=12 cargo test -p baml-rt-a2a stress_quickjs_concurrent_agent_messages -- --ignored --nocapture
//! ```
//!
//! Env:
//! - `QUICKJS_STRESS_N` — concurrent streams (default `8`).
//! - `QUICKJS_STRESS_YIELDS` — `__chat_yield` iterations per stream before `final` (default `8`).

use std::time::{Duration, Instant};

use baml_rt_core::{A2aRequestHandler, A2aWireRequest, collect_a2a_stream_one_shot};
use futures_util::future::join_all;
use test_support::common::{
    build_minimal_a2a_agent_with_stream_idle_secs, chunks_from_responses, send_stream_request,
};
use tokio::time::timeout;

/// Same shape as `quickjs_invoker_send_requirement_test` (known-good on minimal agent).
const STRESS_ON_CHAT: &str = r#"
globalThis.onChatMessage = async function(message) {
    const text = (message && message.parts && message.parts[0] && message.parts[0].text) || "";
    const match = /^stress:(\d+)$/.exec(text);
    const n = match ? parseInt(match[1], 10) : 8;
    for (let i = 0; i < n; i++) {
        __chat_yield({ message: { parts: [{ text: "s-" + i }] } });
    }
    __chat_yield({ final: true });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "manual QuickJS/A2A stress (see module doc); not CI"]
async fn stress_quickjs_concurrent_agent_messages() {
    let n: usize = std::env::var("QUICKJS_STRESS_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let yields: usize = std::env::var("QUICKJS_STRESS_YIELDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    // Per-stream budget: serialized lane → allow ~seconds per stream under load; cap at 15 min.
    let per_stream_cap =
        (30u64 + (n as u64).saturating_mul(yields as u64).saturating_mul(3)).min(900);
    let collect_timeout = Duration::from_secs(per_stream_cap.max(90));

    assert!(n > 0 && n < 10_000, "QUICKJS_STRESS_N out of range");
    assert!(
        yields > 0 && yields < 10_000,
        "QUICKJS_STRESS_YIELDS out of range"
    );

    let agent = build_minimal_a2a_agent_with_stream_idle_secs(STRESS_ON_CHAT, 2).await;

    let wall_start = Instant::now();
    let futs: Vec<_> = (0..n)
        .map(|idx| {
            let agent = agent.clone();
            async move {
                let t0 = Instant::now();
                // Match minimal-agent tests: omit context_id unless isolating turns (live stream).
                let body = format!("stress:{yields}");
                // JSON-RPC `id` must parse as temporal correlation (`corr-<millis>-<counter>`).
                let request = send_stream_request(
                    &format!("msg-stress-{idx}"),
                    &body,
                    &format!("corr-1700000000200-{}", idx),
                    None,
                );
                let stream = agent
                    .handle_a2a_stream(A2aWireRequest::from(request))
                    .await
                    .unwrap_or_else(|e| panic!("handle_a2a_stream idx={idx}: {e}"));
                let collected = timeout(collect_timeout, collect_a2a_stream_one_shot(stream))
                    .await
                    .unwrap_or_else(|_| {
                        panic!(
                            "collect timeout idx={idx} (limit {:?}; raise QUICKJS_STRESS_* or expect single-lane queueing)",
                            collect_timeout
                        )
                    });
                let inner: Vec<_> = collected.into_iter().map(|c| c.into_inner()).collect();
                let nchunks = chunks_from_responses(&inner).len();
                let has_final = inner.iter().any(|r| {
                    r.get("result")
                        .and_then(|x| x.get("final"))
                        .and_then(|f| f.as_bool())
                        == Some(true)
                });
                (idx, t0.elapsed(), nchunks, has_final)
            }
        })
        .collect();

    let results = join_all(futs).await;
    let wall = wall_start.elapsed();

    let mut durations: Vec<Duration> = results.iter().map(|(_, d, _, _)| *d).collect();
    durations.sort();
    let min = *durations.first().expect("non-empty");
    let max = *durations.last().expect("non-empty");
    let sum_ms: u128 = durations.iter().map(|d| d.as_millis()).sum();
    let avg = sum_ms / n as u128;

    let min_chunks = results
        .iter()
        .map(|(_, _, c, _)| *c)
        .min()
        .expect("non-empty");
    let max_chunks = results
        .iter()
        .map(|(_, _, c, _)| *c)
        .max()
        .expect("non-empty");
    let all_final = results.iter().all(|(_, _, _, f)| *f);

    eprintln!(
        "quickjs_agent_message_stress: n={n} yields_per_stream={yields} wall={wall:?} per_stream_ms min={} max={} avg={}",
        min.as_millis(),
        max.as_millis(),
        avg
    );
    eprintln!(
        "quickjs_agent_message_stress: chunks_per_stream min={min_chunks} max={max_chunks} all_final={all_final}"
    );

    assert!(all_final, "every stream must end with a final chunk");
    assert!(
        min_chunks > 0,
        "expected at least one content chunk per stream"
    );

    eprintln!(
        "quickjs_agent_message_stress: straggler_hint max_ms/avg_ms={:.2}",
        max.as_millis() as f64 / avg.max(1) as f64
    );
    eprintln!(
        "quickjs_agent_message_stress: HOTSPOT single QuickJS handover lane — high concurrent N queues JS/stream work (~O(N) streams × yield cost); see baml-rt-quickjs `a2a_stream` docs"
    );
}
