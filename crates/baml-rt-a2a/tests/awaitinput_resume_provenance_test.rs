// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Bug A regression: after `awaitInput` suspends a task, both the assistant question
//! AND the user's follow-up turn must land in the provenance graph as `Message`
//! lifecycle events. Without these writes, conversation history sees only the first
//! user turn forever and the LLM keeps reclassifying the same input.
//!
//! Two coupled bugs are guarded:
//!   - **A.2** `run_live_stream_session` resume branch in
//!     `crates/baml-rt-a2a/src/a2a_transport.rs` injects the follow-up request directly
//!     into the suspended JS via `resume_tx.send(...)` and therefore bypasses the
//!     per-turn `insert_message_for_receiver` write; the fix re-adds that write.
//!   - **A.3** the former wrapper-backed `apply_task_chunk` path only looked at the top-level
//!     `StreamResponse.message`, dropping assistant messages that the QuickJS host
//!     emits inside `statusUpdate.status.message` (the awaitInput shape).
//!
//! Both fixes are exercised end-to-end here through the `task-lifecycle-demo` fixture,
//! which uses `ctx.emit.awaitInput()` natively.

#![recursion_limit = "256"]

use std::{fs, sync::Arc};

use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{A2aWireRequest, collect_a2a_stream_one_shot};
use baml_rt_provenance::{ProvenanceContextReader, SurrealStoreBuilder};
use test_support::common::{
    build_fixture_package_to_temp, ensure_fixture_runtime_types, send_stream_request,
};

async fn drain(agent: &A2aAgent, request: serde_json::Value) -> Vec<baml_rt_core::A2aStreamChunk> {
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await
        .expect("open stream");
    collect_a2a_stream_one_shot(stream).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn awaitinput_two_turn_persists_inbound_and_outbound_messages() {
    ensure_fixture_runtime_types();
    let built = build_fixture_package_to_temp("task-lifecycle-demo").await;
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .load_schema(built.to_str().expect("utf8 path"))
        .expect("load schema");
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("task-lifecycle-demo dist/index.js");

    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("isolated provenance store");

    let agent = A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_surreal_store(store.clone())
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
        .build()
        .await
        .expect("build task-lifecycle-demo agent with surreal store");

    let context_id = baml_rt_core::ids::ContextId::new(20260512, 1);

    let turn1 = send_stream_request(
        "msg-await-1",
        "lifecycle-demo",
        "corr-100-1",
        Some(context_id.clone()),
    );
    let _ = drain(&agent, turn1).await;

    let turn2 = send_stream_request(
        "msg-await-2",
        "review-path",
        "corr-100-2",
        Some(context_id.clone()),
    );
    let _ = drain(&agent, turn2).await;

    // Provenance writes happen on an async pipeline that is not synchronously awaited
    // by the SSE drain; give the writer one tick to settle before asserting.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("read context messages");

    let user_ids: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == "ROLE_USER")
        .map(|m| m.message_id.as_str())
        .collect();
    assert!(
        user_ids.contains(&"msg-await-1"),
        "Bug A: first user turn must be persisted; got user message_ids: {:?}",
        user_ids
    );
    assert!(
        user_ids.contains(&"msg-await-2"),
        "Bug A.2 regression: follow-up user turn (`msg-await-2`) on the same context \
         was not persisted as a Message provenance event. The resume path in \
         `run_live_stream_session` is silently dropping inbound user messages — \
         conversation history will appear stuck on the first turn. \
         got user message_ids: {:?}",
        user_ids
    );

    let agent_messages: Vec<&baml_rt_conversation::view::ProvenanceContextMessage> =
        messages.iter().filter(|m| m.role == "ROLE_AGENT").collect();
    assert!(
        !agent_messages.is_empty(),
        "Bug A.3 regression: at least one assistant message (the awaitInput question \
         emitted via `ctx.emit.awaitInput`) must be persisted as a Message provenance \
         event. None were found — `apply_task_chunk` is dropping nested \
         `statusUpdate.status.message` payloads. messages={:?}",
        messages
    );
}
