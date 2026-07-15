// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use crate::{
    schema::{AnchorSign, ParseArtifact, Template, critical_nodes},
    tier::Tier,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateAction {
    Execute,
    ExecuteFlagged,
    Ask,
    QueueForHuman,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateDecision {
    pub action: GateAction,
    pub tier: u8,
    pub score: f32,
    #[serde(default)]
    pub requests: Vec<String>,
    #[serde(default)]
    pub reason: String,
}

pub trait GatePolicy {
    fn decide(&self, art: &ParseArtifact, tier: Tier) -> GateDecision;
}

fn ranked_requests(art: &ParseArtifact, deficient: &[String]) -> Vec<String> {
    let crit: std::collections::HashSet<&str> = critical_nodes(art.template_kind())
        .iter()
        .copied()
        .collect();
    let mut out = deficient.to_vec();
    out.sort_by(|a, b| {
        let na = art.node(a);
        let nb = art.node(b);
        let ca = na.map(|n| crit.contains(n.name.as_str())).unwrap_or(false);
        let cb = nb.map(|n| crit.contains(n.name.as_str())).unwrap_or(false);
        let ia = na.map(|n| n.interpretations.len()).unwrap_or(0);
        let ib = nb.map(|n| n.interpretations.len()).unwrap_or(0);
        cb.cmp(&ca).then(ia.cmp(&ib).reverse())
    });
    out
}

/// P4 ambiguity-aware gate (production policy).
#[derive(Debug, Clone)]
pub struct AmbiguityAwareGate {
    pub need: [f32; 4],
    pub supp_floor: [f32; 4],
    pub require_verify_t3: bool,
}

impl Default for AmbiguityAwareGate {
    fn default() -> Self {
        Self {
            need: [0.0, 0.3, 0.8, 0.8],
            supp_floor: [0.0, 0.0, 0.0, 0.0],
            require_verify_t3: true,
        }
    }
}

use serde::{Deserialize, Serialize};

impl GatePolicy for AmbiguityAwareGate {
    fn decide(&self, art: &ParseArtifact, tier: Tier) -> GateDecision {
        let t = tier.as_u8() as usize;
        let crit: std::collections::HashSet<&str> = critical_nodes(art.template_kind())
            .iter()
            .copied()
            .collect();
        let mut deficient = Vec::new();
        for node in &art.nodes {
            let ambiguous = node.trojan.is_some() || node.interpretations.len() > 1;
            let need = if crit.contains(node.name.as_str()) || ambiguous {
                self.need[t]
            } else {
                self.supp_floor[t]
            };
            if node.strength(true) < need {
                deficient.push(node.name.clone());
            }
        }
        if tier == Tier::Irreversible
            && self.require_verify_t3
            && art.template_kind() == Template::AgenticExecution
            && let Some(cn) = art.node("CRITERION")
            && !cn.has_sign(AnchorSign::Verify)
            && !deficient.iter().any(|d| d == "CRITERION")
        {
            deficient.push("CRITERION".into());
        }
        let score = art
            .nodes
            .iter()
            .filter(|n| {
                crit.contains(n.name.as_str()) || n.trojan.is_some() || n.interpretations.len() > 1
            })
            .map(|n| n.strength(true))
            .fold(1.0_f32, f32::min);
        if deficient.is_empty() {
            let action = if tier.as_u8() <= 1 {
                GateAction::Execute
            } else {
                GateAction::ExecuteFlagged
            };
            return GateDecision {
                action,
                tier: tier.as_u8(),
                score,
                requests: vec![],
                reason: String::new(),
            };
        }
        let action = if tier == Tier::Irreversible {
            GateAction::QueueForHuman
        } else {
            GateAction::Ask
        };
        GateDecision {
            action,
            tier: tier.as_u8(),
            score,
            requests: ranked_requests(art, &deficient),
            reason: format!("deficient: {deficient:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Anchor, EnvSignals, Node};

    fn grounded_art() -> ParseArtifact {
        ParseArtifact {
            instruction: "archive inactive users".into(),
            template: "agentic_execution".into(),
            nodes: vec![
                Node {
                    name: "OBJECT".into(),
                    anchors: vec![
                        Anchor {
                            sign: AnchorSign::Symbol,
                            content: "users".into(),
                            source: "user".into(),
                        },
                        Anchor {
                            sign: AnchorSign::Icon,
                            content: "schema".into(),
                            source: "user".into(),
                        },
                    ],
                    interpretations: vec![],
                    trojan: None,
                },
                Node {
                    name: "TARGET".into(),
                    anchors: vec![Anchor {
                        sign: AnchorSign::Index,
                        content: "prod.db".into(),
                        source: "user".into(),
                    }],
                    interpretations: vec![],
                    trojan: None,
                },
                Node {
                    name: "ACTION".into(),
                    anchors: vec![Anchor {
                        sign: AnchorSign::Icon,
                        content: "set deleted_at".into(),
                        source: "user".into(),
                    }],
                    interpretations: vec![],
                    trojan: None,
                },
                Node {
                    name: "SCOPE".into(),
                    anchors: vec![Anchor {
                        sign: AnchorSign::Icon,
                        content: "90 days".into(),
                        source: "user".into(),
                    }],
                    interpretations: vec![],
                    trojan: None,
                },
            ],
            env: EnvSignals::default(),
            covers: vec![],
            postconditions: vec![],
        }
    }

    #[test]
    fn grounded_passes_tier2() {
        let g = AmbiguityAwareGate::default();
        let d = g.decide(&grounded_art(), Tier::Mutating);
        assert_eq!(d.action, GateAction::ExecuteFlagged);
        assert!(d.requests.is_empty());
    }

    #[test]
    fn trojan_node_deficient() {
        let g = AmbiguityAwareGate::default();
        let mut art = grounded_art();
        art.nodes[2] = Node {
            name: "ACTION".into(),
            anchors: vec![Anchor {
                sign: AnchorSign::Symbol,
                content: "clean up".into(),
                source: "user".into(),
            }],
            interpretations: vec!["a".into(), "b".into()],
            trojan: Some("clean up".into()),
        };
        let d = g.decide(&art, Tier::Mutating);
        assert!(!d.requests.is_empty());
        assert!(d.requests.contains(&"ACTION".to_string()));
    }
}
