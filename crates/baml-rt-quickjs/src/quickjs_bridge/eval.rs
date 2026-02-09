use baml_rt_core::effects::EffectLiveness;
use baml_rt_core::ids::ContextId;
use std::sync::Arc;

/// Encapsulates effect-gated timeout logic for promise polling.
///
/// Determines timeout attempts based on whether effects are in-flight:
/// - Effects active: use max_attempts (configurable, default 30 minutes) to allow I/O to complete
/// - No effects: use idle_timeout_attempts (default 5s) to detect deadlocks
pub struct EffectGatedPoller {
    liveness: Option<Arc<dyn EffectLiveness>>,
    context_id: Option<ContextId>,
    idle_timeout_attempts: u32,
    max_attempts: u32,
}

impl EffectGatedPoller {
    /// Default maximum attempts when effects are in-flight (30 minutes)
    pub const DEFAULT_MAX_ATTEMPTS: u32 = 1_800_000;

    pub fn new(
        liveness: Option<Arc<dyn EffectLiveness>>,
        context_id: Option<ContextId>,
        idle_timeout_ms: u64,
        max_attempts_ms: u64,
    ) -> Self {
        Self {
            liveness,
            context_id,
            idle_timeout_attempts: idle_timeout_ms as u32,
            max_attempts: max_attempts_ms as u32,
        }
    }

    /// Get the timeout attempts based on current effect state.
    ///
    /// Returns max_attempts if effects are in-flight, otherwise idle_timeout_attempts.
    pub async fn timeout_attempts(&self) -> u32 {
        match (&self.liveness, &self.context_id) {
            (Some(liveness), Some(ctx_id)) => {
                let counts = liveness.in_flight(ctx_id).await;
                if counts.any() {
                    // Effects active: use long timeout
                    self.max_attempts
                } else {
                    // No effects: use short idle timeout
                    self.idle_timeout_attempts
                }
            }
            _ => {
                // No liveness tracker: use long timeout (backward compat)
                self.max_attempts
            }
        }
    }
}
