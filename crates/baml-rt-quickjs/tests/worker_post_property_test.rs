//! Property tests for QuickJS worker fire-and-forget posting.
//!
//! Invariant:
//!   ∀ posted closure c_i: c_i executes exactly once.
//! Liveness:
//!   Every posted closure eventually executes on the QuickJS worker thread.

#![recursion_limit = "256"]

use baml_rt_core::ids::{AgentId, UuidId};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge};
use proptest::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Mutex, oneshot};
use tokio::time::{Duration, timeout};

fn proptest_cfg(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(cases);
    cfg.failure_persistence = None;
    cfg
}

proptest! {
    #![proptest_config(proptest_cfg(6))]

    #[test]
    fn prop_post_to_worker_void_eventually_runs_all_posts(n in 1usize..=20usize) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let runtime = Arc::new(Mutex::new(BamlRuntimeManager::new().expect("runtime")));
            let agent_id =
                AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000f2").unwrap());
            let bridge = QuickJSBridge::new(runtime, agent_id).await.expect("bridge");

            let counter = Arc::new(AtomicUsize::new(0));
            let mut acks = Vec::with_capacity(n);

            for _ in 0..n {
                let counter = Arc::clone(&counter);
                let (tx, rx) = oneshot::channel::<()>();
                bridge.post_to_worker_void(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let _ = tx.send(());
                });
                acks.push(rx);
            }

            for ack in acks {
                timeout(Duration::from_secs(2), ack)
                    .await
                    .expect("worker closure timed out")
                    .expect("worker closure dropped ack");
            }

            assert_eq!(
                counter.load(Ordering::SeqCst),
                n,
                "every posted closure must execute exactly once"
            );
        });
    }
}
