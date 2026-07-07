// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

pub const SIGN_WEIGHTS: &[(&str, f32)] = &[
    ("symbol", 0.3),
    ("index", 0.8),
    ("icon", 0.9),
    ("verify", 1.0),
    ("free", 1.0),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    AgenticExecution,
    Delegation,
    CodeGeneration,
    ConsequentialContent,
    Research,
}

impl Template {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgenticExecution => "agentic_execution",
            Self::Delegation => "delegation",
            Self::CodeGeneration => "code_generation",
            Self::ConsequentialContent => "consequential_content",
            Self::Research => "research",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "delegation" => Self::Delegation,
            "code_generation" => Self::CodeGeneration,
            "consequential_content" => Self::ConsequentialContent,
            "research" => Self::Research,
            _ => Self::AgenticExecution,
        }
    }
}

pub fn critical_nodes(template: Template) -> &'static [&'static str] {
    match template {
        Template::AgenticExecution => &["ACTION", "TARGET", "SCOPE"],
        Template::Delegation => &["CRITERIA", "GOAL"],
        Template::CodeGeneration => &["BEHAVIOR", "INTERFACE"],
        Template::ConsequentialContent => &["AUDIENCE", "OBJECTIVE", "FACTS"],
        Template::Research => &["SUBJECT", "SCOPE"],
    }
}

pub fn sign_weight(sign: &str) -> f32 {
    SIGN_WEIGHTS
        .iter()
        .find(|(k, _)| *k == sign)
        .map(|(_, w)| *w)
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorSign {
    Symbol,
    Index,
    Icon,
    Verify,
    Free,
}

impl AnchorSign {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "symbol" => Some(Self::Symbol),
            "index" => Some(Self::Index),
            "icon" => Some(Self::Icon),
            "verify" => Some(Self::Verify),
            "free" => Some(Self::Free),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Symbol => "symbol",
            Self::Index => "index",
            Self::Icon => "icon",
            Self::Verify => "verify",
            Self::Free => "free",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub sign: AnchorSign,
    #[serde(default)]
    pub content: String,
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub interpretations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trojan: Option<String>,
}

impl Node {
    pub fn strength(&self, trojan_veto: bool) -> f32 {
        let has_disambig = self.anchors.iter().any(|a| {
            matches!(
                a.sign,
                AnchorSign::Icon | AnchorSign::Index | AnchorSign::Verify
            )
        });
        let mut best = 0.0_f32;
        for anchor in &self.anchors {
            let mut w = sign_weight(anchor.sign.as_str());
            if trojan_veto
                && self.trojan.is_some()
                && anchor.sign == AnchorSign::Symbol
                && !has_disambig
            {
                w = 0.0;
            }
            best = best.max(w);
        }
        best
    }

    pub fn has_sign(&self, sign: AnchorSign) -> bool {
        self.anchors.iter().any(|a| a.sign == sign)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvSignals {
    #[serde(default = "default_unknown")]
    pub environment: String,
    #[serde(default = "default_true")]
    pub reversible: bool,
    #[serde(default)]
    pub external_visibility: bool,
    #[serde(default = "default_lookup")]
    pub verb_class: String,
}

fn default_unknown() -> String {
    "unknown".to_string()
}
fn default_true() -> bool {
    true
}
fn default_lookup() -> String {
    "lookup".to_string()
}

impl Default for EnvSignals {
    fn default() -> Self {
        Self {
            environment: default_unknown(),
            reversible: true,
            external_visibility: false,
            verb_class: default_lookup(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Postcondition {
    pub cmd: String,
    #[serde(default)]
    pub desc: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseArtifact {
    pub instruction: String,
    pub template: String,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub env: EnvSignals,
    #[serde(default)]
    pub covers: Vec<String>,
    #[serde(default)]
    pub postconditions: Vec<Postcondition>,
}

impl ParseArtifact {
    pub fn template_kind(&self) -> Template {
        Template::parse(&self.template)
    }

    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    pub fn coerce_from_value(value: &serde_json::Value) -> Self {
        let mut art: Self = serde_json::from_value(value.clone()).unwrap_or_else(|_| Self {
            instruction: String::new(),
            template: "agentic_execution".to_string(),
            nodes: vec![],
            env: EnvSignals::default(),
            covers: vec![],
            postconditions: vec![],
        });
        for node in &mut art.nodes {
            node.trojan = coerce_trojan(node.trojan.take());
        }
        art
    }
}

pub fn coerce_trojan(v: Option<String>) -> Option<String> {
    match v {
        Some(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trojan_veto_zeroes_symbol_only() {
        let n = Node {
            name: "ACTION".into(),
            anchors: vec![Anchor {
                sign: AnchorSign::Symbol,
                content: "clean up".into(),
                source: "user".into(),
            }],
            interpretations: vec!["a".into(), "b".into()],
            trojan: Some("clean up".into()),
        };
        assert_eq!(n.strength(true), 0.0);
        let mut n2 = n.clone();
        n2.anchors.push(Anchor {
            sign: AnchorSign::Icon,
            content: "schema".into(),
            source: "user".into(),
        });
        assert_eq!(n2.strength(true), 0.9);
    }
}
