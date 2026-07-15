// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_semiotic::lint::lint;
use baml_rt_semiotic::schema::{Anchor, AnchorSign, EnvSignals, Node, ParseArtifact};

fn art(instruction: &str, nodes: Vec<Node>) -> ParseArtifact {
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
fn prompt_lint_flags_trojans() {
    let out = lint(art(
        "clean up the inactive users in prod",
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
    ));
    assert!(out.nodes[0].trojan.is_some());
}

#[test]
fn prompt_lint_silent_when_defused() {
    let out = lint(art(
        "delete users with last_login older than 90 days",
        vec![Node {
            name: "SCOPE".into(),
            anchors: vec![Anchor {
                sign: AnchorSign::Symbol,
                content: "older than 90 days".into(),
                source: "user".into(),
            }],
            interpretations: vec![],
            trojan: None,
        }],
    ));
    assert!(out.nodes.iter().all(|n| n.trojan.is_none()));
}
