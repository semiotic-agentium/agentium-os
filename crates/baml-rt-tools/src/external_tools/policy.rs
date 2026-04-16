//! Per-tool invocation policy: concurrency caps, failure tracking, quarantine.
//!
//! Scope is per-agent-instance by default. Quarantine is tripped after
//! `N` consecutive failures and lifted manually (future: via policy decision).
//!
//! Backoff values are advisory: callers decide whether to sleep or surface.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, Semaphore};

use crate::ToolName;

/// V1 default timeout for `tool/describe`.
pub const DEFAULT_DESCRIBE_TIMEOUT: Duration = Duration::from_secs(5);

/// V1 default timeout for `tool/invoke`.
pub const DEFAULT_INVOKE_TIMEOUT: Duration = Duration::from_secs(30);

/// V1 default max concurrent invocations per tool (per agent instance).
pub const DEFAULT_MAX_CONCURRENT: usize = 10;

/// V1 default quarantine threshold (consecutive failures).
pub const DEFAULT_QUARANTINE_THRESHOLD: u32 = 5;

/// Backoff schedule used on consecutive failures.
pub const BACKOFF_SCHEDULE_MS: &[u64] = &[1_000, 2_000, 4_000, 8_000, 16_000, 30_000];

/// Per-tool resource quota (runner-side enforcement).
#[derive(Debug, Clone)]
pub struct ToolQuota {
    pub max_concurrent: usize,
    pub describe_timeout: Duration,
    pub invoke_timeout: Duration,
    pub quarantine_threshold: u32,
}

impl Default for ToolQuota {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            describe_timeout: DEFAULT_DESCRIBE_TIMEOUT,
            invoke_timeout: DEFAULT_INVOKE_TIMEOUT,
            quarantine_threshold: DEFAULT_QUARANTINE_THRESHOLD,
        }
    }
}

/// Current quarantine state for a single tool.
#[derive(Debug, Clone)]
pub enum QuarantineState {
    Healthy,
    Quarantined {
        since: Instant,
        consecutive_failures: u32,
        reason: String,
    },
}

/// Failure cases relevant to policy enforcement.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PolicyError {
    #[error("tool '{tool}' is quarantined: {reason}")]
    Quarantined { tool: ToolName, reason: String },
    #[error("failed to acquire concurrency slot for tool '{tool}'")]
    ConcurrencyExhausted { tool: ToolName },
}

/// Per-tool policy enforcement: semaphore + failure counter + quarantine flag.
pub struct InvocationPolicy {
    tool_name: ToolName,
    quota: ToolQuota,
    semaphore: Arc<Semaphore>,
    state: Arc<Mutex<InvocationState>>,
}

struct InvocationState {
    consecutive_failures: u32,
    quarantine: QuarantineState,
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
                quarantine: QuarantineState::Healthy,
            })),
        }
    }

    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    pub fn quota(&self) -> &ToolQuota {
        &self.quota
    }

    /// Reserve a concurrency slot. Fails closed if the tool is quarantined.
    /// The returned permit releases the slot when dropped.
    pub async fn acquire(
        &self,
    ) -> std::result::Result<tokio::sync::OwnedSemaphorePermit, PolicyError> {
        {
            let state = self.state.lock().await;
            if let QuarantineState::Quarantined { reason, .. } = &state.quarantine {
                return Err(PolicyError::Quarantined {
                    tool: self.tool_name.clone(),
                    reason: reason.clone(),
                });
            }
        }
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PolicyError::ConcurrencyExhausted {
                tool: self.tool_name.clone(),
            })
    }

    /// Record a successful invocation. Resets failure counters.
    pub async fn record_success(&self) {
        let mut state = self.state.lock().await;
        state.consecutive_failures = 0;
        // Note: success does NOT automatically lift quarantine.
        // Callers must explicitly call `lift_quarantine()` — prevents flapping.
    }

    /// Record a failed invocation. Trips quarantine if threshold hit.
    /// Returns the suggested backoff to sleep before the next attempt.
    pub async fn record_failure(&self, reason: impl Into<String>) -> Duration {
        let mut state = self.state.lock().await;
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);

        if matches!(state.quarantine, QuarantineState::Healthy)
            && state.consecutive_failures >= self.quota.quarantine_threshold
        {
            state.quarantine = QuarantineState::Quarantined {
                since: Instant::now(),
                consecutive_failures: state.consecutive_failures,
                reason: reason.into(),
            };
        }

        backoff_for(state.consecutive_failures)
    }

    /// Manually lift quarantine (future: exposed via policy-decision API).
    pub async fn lift_quarantine(&self) {
        let mut state = self.state.lock().await;
        state.quarantine = QuarantineState::Healthy;
        state.consecutive_failures = 0;
    }

    pub async fn current_state(&self) -> QuarantineState {
        self.state.lock().await.quarantine.clone()
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
    async fn quarantine_after_threshold() {
        let policy = InvocationPolicy::new(
            tool(),
            ToolQuota {
                quarantine_threshold: 3,
                ..Default::default()
            },
        );
        for _ in 0..3 {
            policy.record_failure("boom").await;
        }
        match policy.current_state().await {
            QuarantineState::Quarantined { .. } => {}
            other => panic!("expected quarantined, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn success_resets_counter_but_not_quarantine() {
        let policy = InvocationPolicy::new(
            tool(),
            ToolQuota {
                quarantine_threshold: 2,
                ..Default::default()
            },
        );
        policy.record_failure("first").await;
        policy.record_failure("second").await;
        // Quarantined now.
        policy.record_success().await;
        match policy.current_state().await {
            QuarantineState::Quarantined { .. } => {}
            _ => panic!("success must not auto-lift quarantine"),
        }
        policy.lift_quarantine().await;
        assert!(matches!(
            policy.current_state().await,
            QuarantineState::Healthy
        ));
    }

    #[tokio::test]
    async fn acquire_fails_when_quarantined() {
        let policy = InvocationPolicy::new(
            tool(),
            ToolQuota {
                quarantine_threshold: 1,
                ..Default::default()
            },
        );
        policy.record_failure("boom").await;
        let err = policy.acquire().await.err().expect("must fail closed");
        assert!(matches!(err, PolicyError::Quarantined { .. }));
    }

    #[test]
    fn backoff_progresses() {
        assert_eq!(backoff_for(1), Duration::from_millis(1_000));
        assert_eq!(backoff_for(3), Duration::from_millis(4_000));
        assert_eq!(backoff_for(10), Duration::from_millis(30_000));
    }
}
