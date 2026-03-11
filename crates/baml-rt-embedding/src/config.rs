//! Configuration for the drift detection interceptor.

use std::collections::HashSet;

/// Operating mode for the drift detector.
///
/// Using an enum (not a bool) to avoid boolean blindness and to leave room
/// for future modes (e.g. `Sample(f32)` for probabilistic enforcement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftMode {
    /// Log drift events via `tracing` without blocking any calls.
    /// Safe default for initial rollout and threshold calibration.
    Audit,
    /// Log **and** block the next LLM call in the same ReAct loop when
    /// the drift score falls below `block_threshold`.
    Enforce,
}

/// Configuration for [`super::DriftDetectorInterceptor`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DriftConfig {
    /// Cosine similarity below this emits a `tracing::warn!`.
    /// Default: `0.5`.
    pub warn_threshold: f32,

    /// Cosine similarity below this triggers a block (in [`DriftMode::Enforce`]).
    /// Must be ≤ `warn_threshold`.  Default: `0.25`.
    pub block_threshold: f32,

    /// Whether to log-only or log-and-block.
    pub mode: DriftMode,

    /// When `Some`, only these function names are monitored.
    /// When `None`, all functions are monitored (minus `skip_functions`).
    #[serde(default)]
    pub monitored_functions: Option<HashSet<String>>,

    /// Functions to always skip (e.g. `"PlanCoordinatorWorkflow"` — the planner
    /// itself is trusted and doesn't process untrusted data in the same way).
    #[serde(default)]
    pub skip_functions: HashSet<String>,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            warn_threshold: 0.5,
            block_threshold: 0.25,
            mode: DriftMode::Audit,
            monitored_functions: None,
            skip_functions: HashSet::new(),
        }
    }
}

impl DriftConfig {
    /// Returns `true` if `function_name` should be monitored according to the
    /// current allowlist / denylist configuration.
    pub fn should_monitor(&self, function_name: &str) -> bool {
        if self.skip_functions.contains(function_name) {
            return false;
        }
        match &self.monitored_functions {
            Some(allowed) => allowed.contains(function_name),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_monitors_all_functions() {
        let cfg = DriftConfig::default();
        assert!(cfg.should_monitor("ChooseClickUpAction"));
        assert!(cfg.should_monitor("ChooseNotionAction"));
    }

    #[test]
    fn skip_functions_are_excluded() {
        let cfg = DriftConfig {
            skip_functions: HashSet::from(["PlanCoordinatorWorkflow".to_owned()]),
            ..Default::default()
        };
        assert!(!cfg.should_monitor("PlanCoordinatorWorkflow"));
        assert!(cfg.should_monitor("ChooseClickUpAction"));
    }

    #[test]
    fn monitored_functions_allowlist() {
        let cfg = DriftConfig {
            monitored_functions: Some(HashSet::from(["ChooseClickUpAction".to_owned()])),
            ..Default::default()
        };
        assert!(cfg.should_monitor("ChooseClickUpAction"));
        assert!(!cfg.should_monitor("ChooseNotionAction"));
    }

    #[test]
    fn skip_takes_precedence_over_allowlist() {
        let cfg = DriftConfig {
            monitored_functions: Some(HashSet::from(["Foo".to_owned()])),
            skip_functions: HashSet::from(["Foo".to_owned()]),
            ..Default::default()
        };
        assert!(!cfg.should_monitor("Foo"));
    }

    #[test]
    fn default_mode_is_audit() {
        assert_eq!(DriftConfig::default().mode, DriftMode::Audit);
    }
}
