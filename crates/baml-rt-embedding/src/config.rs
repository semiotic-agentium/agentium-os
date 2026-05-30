// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Configuration for drift assessment.

use std::collections::HashSet;

/// Operating mode for drift handling.
///
/// Using an enum (not a bool) to avoid boolean blindness and to leave room
/// for future modes (e.g. `Sample(f32)` for probabilistic enforcement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftMode {
    #[default]
    /// Log drift events via `tracing` without blocking any calls.
    /// Safe default for initial rollout and threshold calibration.
    Audit,
    /// Log **and** block the next LLM call in the same ReAct loop when
    /// the drift score falls below `block_min_score`.
    Enforce,
}

/// Configuration for drift scoring and threshold classification.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DriftConfig {
    /// Cosine similarity below this emits a `tracing::warn!`.
    /// Default: `0.5`.
    pub warn_min_score: f32,

    /// Cosine similarity below this triggers a block (in [`DriftMode::Enforce`]).
    /// Must be ≤ `warn_min_score`.  Default: `0.25`.
    pub block_min_score: f32,

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
            warn_min_score: 0.5,
            block_min_score: 0.25,
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
    fn should_monitor_respects_defaults_allowlist_and_skip_precedence() {
        // Default: monitors everything, mode is Audit.
        let cfg = DriftConfig::default();
        assert_eq!(cfg.mode, DriftMode::Audit);
        assert!(cfg.should_monitor("ChooseClickUpAction"));
        assert!(cfg.should_monitor("AnyFunction"));

        // Allowlist restricts to named functions only.
        let cfg = DriftConfig {
            monitored_functions: Some(HashSet::from(["ChooseClickUpAction".to_owned()])),
            ..Default::default()
        };
        assert!(cfg.should_monitor("ChooseClickUpAction"));
        assert!(!cfg.should_monitor("ChooseNotionAction"));

        // Skip takes precedence over allowlist.
        let cfg = DriftConfig {
            monitored_functions: Some(HashSet::from(["Foo".to_owned()])),
            skip_functions: HashSet::from(["Foo".to_owned()]),
            ..Default::default()
        };
        assert!(!cfg.should_monitor("Foo"));
    }
}
