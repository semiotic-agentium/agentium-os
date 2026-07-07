// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Config bundle name in the runner config store (`GET/PUT /config/semiotic`).
pub const SEMIOTIC_CONFIG_BUNDLE_NAME: &str = "semiotic";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemioticMode {
    #[default]
    DryRun,
    Enforce,
}

/// Operator-facing gate posture derived from [`SemioticPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemioticPosture {
    Off,
    Audit,
    Enforce,
}

/// Resolved policy for one agent package (Settings / activity surfaces).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveAgentPolicy {
    pub agent_package: String,
    pub has_override: bool,
    pub policy: SemioticPolicy,
    pub posture: SemioticPosture,
    pub summary: String,
}

/// System default with derived posture for operator APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveSystemPolicy {
    pub policy: SemioticPolicy,
    pub posture: SemioticPosture,
    pub summary: String,
}

/// Effective gate policy for one agent (or the global default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: SemioticMode,
    /// Minimum tier to enforce (2 = write-access tools).
    #[serde(default = "default_enforce_min")]
    pub enforce_min_tier: u8,
    #[serde(default = "default_true")]
    pub require_postconditions_t3: bool,
    #[serde(default = "default_true")]
    pub strict_citation_anchors: bool,
}

/// Per-agent overrides keyed by `agent_package` (same as LLM routing / GET /agents).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticOverrides {
    #[serde(default)]
    pub agent: HashMap<String, SemioticPolicy>,
}

/// Full semiotic bundle: global default + per-agent overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticConfig {
    #[serde(flatten)]
    pub default: SemioticPolicy,
    #[serde(default)]
    pub overrides: SemioticOverrides,
}

fn default_enforce_min() -> u8 {
    2
}
fn default_true() -> bool {
    true
}

impl Default for SemioticPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: SemioticMode::DryRun,
            enforce_min_tier: 2,
            require_postconditions_t3: true,
            strict_citation_anchors: true,
        }
    }
}

impl SemioticPolicy {
    pub fn should_enforce(&self, tier: u8) -> bool {
        self.enabled && self.mode == SemioticMode::Enforce && tier >= self.enforce_min_tier
    }

    pub fn posture(&self) -> SemioticPosture {
        if !self.enabled {
            return SemioticPosture::Off;
        }
        if self.mode == SemioticMode::Enforce {
            SemioticPosture::Enforce
        } else {
            SemioticPosture::Audit
        }
    }

    pub fn summary_label(&self) -> String {
        let posture = match self.posture() {
            SemioticPosture::Off => "Off",
            SemioticPosture::Audit => "Audit",
            SemioticPosture::Enforce => "Enforce",
        };
        let mut parts = vec![format!("{posture} · tier≥{}", self.enforce_min_tier)];
        if self.require_postconditions_t3 {
            parts.push("T3 postconditions".to_string());
        }
        if self.strict_citation_anchors {
            parts.push("strict anchors".to_string());
        }
        parts.join(" · ")
    }
}

impl SemioticConfig {
    pub fn from_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }

    /// Resolve policy for an executing agent. Inheritance: `overrides.agent[package]` → global default.
    pub fn resolve(&self, agent_package: Option<&str>) -> SemioticPolicy {
        if let Some(pkg) = agent_package
            && let Some(policy) = self.overrides.agent.get(pkg)
        {
            return policy.clone();
        }
        self.default.clone()
    }

    pub fn effective_system(&self) -> EffectiveSystemPolicy {
        let policy = self.default.clone();
        let posture = policy.posture();
        let summary = policy.summary_label();
        EffectiveSystemPolicy {
            policy,
            posture,
            summary,
        }
    }

    /// Resolved policies for discovered agents plus any override-only packages.
    pub fn effective_agents(&self, discovered_packages: &[String]) -> Vec<EffectiveAgentPolicy> {
        let mut packages: Vec<String> = discovered_packages.to_vec();
        for key in self.overrides.agent.keys() {
            if !packages.iter().any(|p| p == key) {
                packages.push(key.clone());
            }
        }
        packages.sort();
        packages.dedup();
        packages
            .into_iter()
            .map(|agent_package| {
                let has_override = self.overrides.agent.contains_key(&agent_package);
                let policy = self.resolve(Some(&agent_package));
                let posture = policy.posture();
                let summary = policy.summary_label();
                EffectiveAgentPolicy {
                    agent_package,
                    has_override,
                    policy,
                    posture,
                    summary,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_agent_override_when_present() {
        let mut config = SemioticConfig::default();
        config.overrides.agent.insert(
            "deploy-agent".to_string(),
            SemioticPolicy {
                enabled: true,
                mode: SemioticMode::Enforce,
                enforce_min_tier: 2,
                ..Default::default()
            },
        );
        let policy = config.resolve(Some("deploy-agent"));
        assert!(policy.enabled);
        assert_eq!(policy.mode, SemioticMode::Enforce);
    }

    #[test]
    fn resolve_falls_back_to_global_default() {
        let config = SemioticConfig::default();
        let policy = config.resolve(Some("other-agent"));
        assert!(!policy.enabled);
        assert_eq!(policy.mode, SemioticMode::DryRun);
    }

    #[test]
    fn posture_off_when_disabled() {
        let policy = SemioticPolicy::default();
        assert_eq!(policy.posture(), SemioticPosture::Off);
    }

    #[test]
    fn posture_audit_when_dry_run_enabled() {
        let policy = SemioticPolicy {
            enabled: true,
            mode: SemioticMode::DryRun,
            ..Default::default()
        };
        assert_eq!(policy.posture(), SemioticPosture::Audit);
    }

    #[test]
    fn effective_agents_marks_override() {
        let mut config = SemioticConfig::default();
        config.overrides.agent.insert(
            "custom-agent".to_string(),
            SemioticPolicy {
                enabled: true,
                mode: SemioticMode::Enforce,
                ..Default::default()
            },
        );
        let agents = config.effective_agents(&["slack-agent".to_string()]);
        assert_eq!(agents.len(), 2);
        let custom = agents
            .iter()
            .find(|a| a.agent_package == "custom-agent")
            .expect("custom");
        assert!(custom.has_override);
        assert_eq!(custom.posture, SemioticPosture::Enforce);
    }
}
