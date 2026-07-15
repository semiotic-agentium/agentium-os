// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use crate::{
    schema::{ParseArtifact, SIGN_WEIGHTS},
    trojan,
};

pub fn lint(mut art: ParseArtifact) -> ParseArtifact {
    let text = art.instruction.to_lowercase();
    let detected: std::collections::HashSet<String> = trojan::detect(&text).into_iter().collect();
    for node in &mut art.nodes {
        if let Some(ref ph) = node.trojan {
            let ph_l = ph.to_lowercase();
            if text.contains(&ph_l)
                && !detected.contains(&ph_l)
                && trojan::TROJANS.contains(&ph_l.as_str())
            {
                node.trojan = None;
            }
        }
        if node.trojan.is_none() {
            for ph in &detected {
                let sym: String = node
                    .anchors
                    .iter()
                    .filter(|a| a.sign == crate::schema::AnchorSign::Symbol)
                    .map(|a| a.content.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                let has_disambig = node.anchors.iter().any(|a| {
                    matches!(
                        a.sign,
                        crate::schema::AnchorSign::Icon
                            | crate::schema::AnchorSign::Index
                            | crate::schema::AnchorSign::Verify
                    )
                });
                if sym.contains(ph.as_str()) && !has_disambig {
                    node.trojan = Some(ph.clone());
                    break;
                }
            }
        }
    }
    art
}

#[allow(dead_code)]
pub fn valid_sign(s: &str) -> bool {
    SIGN_WEIGHTS.iter().any(|(k, _)| *k == s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Anchor, AnchorSign, EnvSignals, Node, ParseArtifact};

    fn base_art(instruction: &str, nodes: Vec<Node>) -> ParseArtifact {
        ParseArtifact {
            instruction: instruction.into(),
            template: "agentic_execution".into(),
            nodes,
            env: EnvSignals::default(),
            covers: vec![],
            postconditions: vec![],
        }
    }

    #[test]
    fn tags_trojan_on_ambiguous_symbol() {
        let art = base_art(
            "clean up inactive users",
            vec![Node {
                name: "ACTION".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Symbol,
                    content: "clean up".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            }],
        );
        let out = lint(art);
        assert_eq!(out.nodes[0].trojan.as_deref(), Some("clean up"));
    }

    #[test]
    fn defuses_quantified_instruction() {
        let art = base_art(
            "delete users inactive for more than 90 days",
            vec![Node {
                name: "SCOPE".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Symbol,
                    content: "inactive".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: Some("inactive".into()),
            }],
        );
        let out = lint(art);
        assert!(out.nodes[0].trojan.is_none());
    }
}
