// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Per-tool invocation policy: concurrency caps and failure backoff.
//!
//! Scope is per-agent-instance by default.
//!
//! Backoff values are advisory: callers decide whether to sleep or surface.

use std::{sync::Arc, time::Duration};

use tokio::sync::{Mutex, Semaphore};

use crate::ToolName;

/// V1 default timeout for `tool/describe` (spawn + JSON-RPC handshake).
///
/// Parallel CI / `cargo test --workspace` can exhaust fork bandwidth; keep headroom beyond a
/// tight 5s so dev-mode fixture tools still complete their first describe under load.
pub const DEFAULT_DESCRIBE_TIMEOUT: Duration = Duration::from_secs(15);

/// V1 default timeout for `tool/invoke`.
pub const DEFAULT_INVOKE_TIMEOUT: Duration = Duration::from_secs(30);

/// V1 default max concurrent invocations per tool (per agent instance).
pub const DEFAULT_MAX_CONCURRENT: usize = 10;

/// Backoff schedule used on consecutive failures.
pub const BACKOFF_SCHEDULE_MS: &[u64] = &[1_000, 2_000, 4_000, 8_000, 16_000, 30_000];

/// Per-tool resource quota (runner-side enforcement).
#[derive(Debug, Clone)]
pub struct ToolQuota {
    pub max_concurrent: usize,
    pub describe_timeout: Duration,
    pub invoke_timeout: Duration,
}

impl Default for ToolQuota {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            describe_timeout: DEFAULT_DESCRIBE_TIMEOUT,
            invoke_timeout: DEFAULT_INVOKE_TIMEOUT,
        }
    }
}

/// Failure cases relevant to policy enforcement.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PolicyError {
    #[error("failed to acquire concurrency slot for tool '{tool}'")]
    ConcurrencyExhausted { tool: ToolName },
}

/// Per-tool policy enforcement: semaphore and failure backoff.
pub struct InvocationPolicy {
    tool_name: ToolName,
    quota: ToolQuota,
    semaphore: Arc<Semaphore>,
    state: Arc<Mutex<InvocationState>>,
}

struct InvocationState {
    consecutive_failures: u32,
}

impl InvocationPolicy {
    pub fn new(tool_name: ToolName, quota: ToolQuota) -> Self {
        let semaphore = Arc::new(Semaphore::new(quota.max_concurrent));
        Self {
            tool_name,
            quota,
            semaphore,
            state: Arc::new(Mutex::new(InvocationState {
                consecutive_failures: 0,
            })),
        }
    }

    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    pub fn quota(&self) -> &ToolQuota {
        &self.quota
    }

    /// Reserve a concurrency slot.
    /// The returned permit releases the slot when dropped.
    pub async fn acquire(
        &self,
    ) -> std::result::Result<tokio::sync::OwnedSemaphorePermit, PolicyError> {
        self.semaphore.clone().acquire_owned().await.map_err(|_| {
            PolicyError::ConcurrencyExhausted {
                tool: self.tool_name.clone(),
            }
        })
    }

    /// Record a successful invocation and reset the advisory backoff.
    pub async fn record_success(&self) {
        self.state.lock().await.consecutive_failures = 0;
    }

    /// Record a failed invocation and return the suggested advisory backoff.
    pub async fn record_failure(&self) -> Duration {
        let mut state = self.state.lock().await;
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        backoff_for(state.consecutive_failures)
    }
}

fn backoff_for(consecutive_failures: u32) -> Duration {
    let idx = (consecutive_failures.saturating_sub(1) as usize).min(BACKOFF_SCHEDULE_MS.len() - 1);
    Duration::from_millis(BACKOFF_SCHEDULE_MS[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ToolName {
        ToolName::parse("support/sample").unwrap()
    }

    #[tokio::test]
    async fn backoff_resets_after_success() {
        let policy = InvocationPolicy::new(tool(), ToolQuota::default());
        assert_eq!(policy.record_failure().await, Duration::from_millis(1_000));
        assert_eq!(policy.record_failure().await, Duration::from_millis(2_000));
        policy.record_success().await;
        assert_eq!(policy.record_failure().await, Duration::from_millis(1_000));
    }

    #[tokio::test]
    async fn acquire_only_enforces_concurrency() {
        let policy = InvocationPolicy::new(
            tool(),
            ToolQuota {
                max_concurrent: 1,
                ..Default::default()
            },
        );
        let permit = policy.acquire().await.expect("first slot available");
        let second = tokio::time::timeout(Duration::from_millis(10), policy.acquire()).await;
        assert!(second.is_err(), "second slot should wait for the first");
        drop(permit);
        let _permit = policy.acquire().await.expect("slot released");
    }

    #[test]
    fn backoff_progresses() {
        assert_eq!(backoff_for(1), Duration::from_millis(1_000));
        assert_eq!(backoff_for(3), Duration::from_millis(4_000));
        assert_eq!(backoff_for(10), Duration::from_millis(30_000));
    }
}
