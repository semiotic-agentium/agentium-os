//! Microbenchmark for the `MemoryManager::add` hot path across graph sizes.
//!
//! Fixes the regression target for the snapshot-serialization work: `add` should perform a
//! single O(graph_size) serialization (the persist write) per success-path call, with the
//! rollback snapshot reduced to an `Arc` clone. The benchmark measures one `add` against a
//! graph pre-populated to 10², 10³, and 10⁴ nodes, so the per-call cost is read at the sizes
//! where serialization dominates.
//!
//! Run with `cargo bench -p baml-tools-memory`. Not part of the default test/nextest path.

use std::path::{Path, PathBuf};

use baml_tools_memory::{
    manager::MemoryManager,
    types::{MemoryAddSendInput, MemoryEventInput, MemoryEventType},
};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

fn one_fact(content: String) -> MemoryAddSendInput {
    MemoryAddSendInput {
        events: vec![MemoryEventInput {
            event_type: MemoryEventType::Fact,
            content,
            session_id: Some(1),
            confidence: Some(0.9),
        }],
        edges: None,
    }
}

/// Build a memory file pre-populated with `node_count` facts and return its path.
fn build_template(rt: &tokio::runtime::Runtime, dir: &Path, node_count: usize) -> PathBuf {
    let path = dir.join(format!("template-{node_count}.amem"));
    let mgr = MemoryManager::open_at(path.clone()).expect("open template");
    rt.block_on(async {
        for i in 0..node_count {
            mgr.add(one_fact(format!("template fact {i}")))
                .await
                .expect("seed add");
        }
    });
    // Drop the manager so its file lock is released before the bench reopens copies.
    drop(mgr);
    path
}

fn bench_add(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let dir = tempfile::tempdir().expect("tempdir");

    let mut group = c.benchmark_group("memory_add");
    for &node_count in &[100usize, 1_000, 10_000] {
        let template = build_template(&rt, dir.path(), node_count);
        // One reusable working file per size. `PerIteration` guarantees the prior manager
        // is dropped (file lock released) before the next setup overwrites it, so exactly
        // one manager and one `.amem` copy ever exist at a time — no FD or disk pileup.
        let work_path = dir.path().join(format!("bench-{node_count}.amem"));
        group.bench_with_input(
            BenchmarkId::from_parameter(node_count),
            &node_count,
            |b, _| {
                b.iter_batched(
                    || {
                        // Restore the N-node graph for each timed `add` (untimed setup).
                        std::fs::copy(&template, &work_path).expect("copy template");
                        MemoryManager::open_at(work_path.clone()).expect("open copy")
                    },
                    |mgr| {
                        rt.block_on(mgr.add(one_fact("benchmark add".to_string())))
                            .expect("add");
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_add);
criterion_main!(benches);
