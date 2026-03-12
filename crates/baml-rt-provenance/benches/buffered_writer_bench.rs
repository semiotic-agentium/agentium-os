//! Criterion benchmarks comparing direct vs buffered provenance writes.
//!
//! Measures wall-clock time for N `add_event` calls through:
//! - **Direct writer**: `GraphqliteProvenanceStore` (each write awaits SQLite commit)
//! - **Buffered writer**: `BufferedProvenanceWriter` wrapping the same store (fire-and-forget + flush)
//!
//! Both use file-backed stores (unique temp paths) so the benchmark captures real
//! SQLite I/O including WAL + synchronous=NORMAL (R1) and batched transactions (R2).

use std::sync::Arc;

use baml_rt_core::{
    Outcome,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, BufferedProvenanceWriter, GraphqliteStoreBuilder, LlmUsage, ProvEvent,
    ProvenanceWriter,
};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::json;

/// Build a file-backed store at a unique temp path.
fn build_file_store() -> Arc<baml_rt_provenance::GraphqliteProvenanceStore> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.into_path().join("bench-provenance.db");
    GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build file-backed store")
}

/// Generate a realistic sequence of provenance events for one ReAct loop iteration:
/// task_exists → task_execution_started → message_received → llm_call_completed → tool_call_completed → message_sent
fn generate_events(n: usize) -> Vec<ProvEvent> {
    let context_id = ContextId::new(1_700_000_000_000, 1);
    let task_id = TaskId::from_external(ExternalId::new("bench-task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());

    let mut events = Vec::with_capacity(n);
    // Seed events (written once, not part of the measured loop)
    events.push(ProvEvent::agent_booted(
        agent_id.clone(),
        AgentType::new("bench_agent").expect("agent_type"),
        "1.0.0".to_string(),
        "bench@1.0.0".to_string(),
    ));
    events.push(ProvEvent::task_exists(
        context_id.clone(),
        task_id.clone(),
    ));
    events.push(ProvEvent::task_execution_started(
        context_id.clone(),
        task_id.clone(),
        agent_id.clone(),
    ));

    // Generate N-3 mixed events (message + llm + tool cycles)
    let remaining = n.saturating_sub(3);
    for i in 0..remaining {
        let event = match i % 4 {
            0 => ProvEvent::message_received_task(
                context_id.clone(),
                task_id.clone(),
                MessageId::from_external(ExternalId::new(format!("msg-user-{i}"))),
                "user".to_string(),
                vec![format!("bench input message {i}")],
                None,
                agent_id.clone(),
                1_700_000_000_000 + i as u64,
            ),
            1 => ProvEvent::llm_call_completed_task(
                context_id.clone(),
                task_id.clone(),
                "DefaultClient".to_string(),
                "openai-generic".to_string(),
                "BenchFunction".to_string(),
                json!({"messages": [{"role": "system", "content": "bench"}]}),
                json!({
                    "agent_id": "00000000-0000-0000-0000-000000000099",
                    "task_id": "bench-task-1",
                    "message_id": format!("msg-user-{i}")
                }),
                LlmUsage::Unknown,
                500,
                Outcome::Success,
            ),
            2 => ProvEvent::tool_call_completed_task(
                context_id.clone(),
                task_id.clone(),
                "bench/tool".to_string(),
                None,
                json!({"action": "BenchAction"}),
                json!({
                    "phase": "execute",
                    "agent_id": "00000000-0000-0000-0000-000000000099",
                    "task_id": "bench-task-1",
                    "message_id": format!("msg-user-{i}"),
                    "result": {"status": "ok"}
                }),
                200,
                Outcome::Success,
                None,
            ),
            _ => ProvEvent::message_sent_task(
                context_id.clone(),
                task_id.clone(),
                MessageId::from_external(ExternalId::new(format!("msg-agent-{i}"))),
                "ROLE_AGENT".to_string(),
                vec![format!("bench response {i}")],
                None,
                agent_id.clone(),
                1_700_000_000_000 + i as u64,
            ),
        };
        events.push(event);
    }
    events
}

fn bench_provenance_writes(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("provenance_write");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    for event_count in [10, 50, 100] {
        let events = generate_events(event_count);

        group.bench_with_input(
            BenchmarkId::new("direct", event_count),
            &events,
            |b, events| {
                b.iter(|| {
                    // Fresh store per iteration to avoid accumulation effects.
                    let store = build_file_store();
                    let writer: Arc<dyn ProvenanceWriter> = store;
                    rt.block_on(async {
                        for event in events {
                            let _ = writer.add_event(event.clone()).await;
                        }
                    });
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("buffered", event_count),
            &events,
            |b, events| {
                b.iter(|| {
                    let store = build_file_store();
                    let buffered: Arc<dyn ProvenanceWriter> =
                        Arc::new(BufferedProvenanceWriter::new(store));
                    rt.block_on(async {
                        for event in events {
                            let _ = buffered.add_event(event.clone()).await;
                        }
                        // Flush to ensure all events are written before the store drops.
                        // This measures total throughput including drain time.
                        let reader: &dyn baml_rt_provenance::ProvenanceContextReader = &*buffered;
                        let _ = reader
                            .context_messages(
                                &ContextId::new(0, 0),
                                Some(0),
                            )
                            .await;
                    });
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_provenance_writes);
criterion_main!(benches);
