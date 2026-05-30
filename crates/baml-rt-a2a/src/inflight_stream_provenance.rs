// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tracks inflight `store_result` tasks spawned from the live transport drain loop so hosts can
//! wait for graceful shutdown (SIGINT/SIGTERM) without relying on panic/SIGKILL durability.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::task::JoinSet;

/// Counts live-transport `store_result` spawns until each completes. Clone is cheap (Arc).
#[derive(Clone)]
pub struct InflightStreamProvenance {
    inflight: Arc<AtomicUsize>,
}

impl InflightStreamProvenance {
    pub fn new() -> Self {
        Self {
            inflight: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Spawn on `join_set` and track until the future completes (including on panic — counter decrements in Drop).
    pub fn join_set_spawn_tracked<F>(&self, join_set: &mut JoinSet<()>, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        let c = self.inflight.clone();
        join_set.spawn(async move {
            struct Dec(Arc<AtomicUsize>);
            impl Drop for Dec {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _dec = Dec(c);
            fut.await;
        });
    }

    /// Poll until inflight hits zero or `timeout` elapses. Returns `true` if idle before timeout.
    pub async fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.inflight.load(Ordering::SeqCst) == 0 {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Default for InflightStreamProvenance {
    fn default() -> Self {
        Self::new()
    }
}
