// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Oracle test: corpus ground-truth labels vs P4 ambiguity-aware gate decisions.

use baml_rt_semiotic::{
    gate::{AmbiguityAwareGate, GateAction, GatePolicy},
    schema::{Anchor, AnchorSign, EnvSignals, Node, ParseArtifact},
    tier::Tier,
};
use serde_json::Value;

const CORPUS: &str = include_str!("../../../tests/fixtures/semiotic/corpus.json");

fn corpus_entry_to_artifact(entry: &Value) -> ParseArtifact {
    let nodes = entry["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|n| {
            let anchors = n["anchors"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|a| {
                    AnchorSign::parse(a["sign"].as_str().unwrap_or("symbol")).map(|sign| Anchor {
                        sign,
                        content: a["content"].as_str().unwrap_or("").into(),
                        source: "corpus".into(),
                    })
                })
                .collect();
            let interpretations: Vec<String> = n["interpretations"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|i| {
                            i.as_str()
                                .map(String::from)
                                .or_else(|| i.get("id").and_then(|v| v.as_str()).map(String::from))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Node {
                name: n["name"].as_str().unwrap_or("").into(),
                anchors,
                interpretations,
                trojan: n["trojan"].as_str().map(String::from),
            }
        })
        .collect();

    let env_obj = &entry["env"];
    ParseArtifact {
        instruction: entry["instruction"].as_str().unwrap_or("").into(),
        template: entry["template"]
            .as_str()
            .unwrap_or("agentic_execution")
            .into(),
        nodes,
        env: EnvSignals {
            environment: env_obj["environment"].as_str().unwrap_or("unknown").into(),
            reversible: env_obj["reversible"].as_bool().unwrap_or(true),
            external_visibility: env_obj["external_visibility"].as_bool().unwrap_or(false),
            verb_class: env_obj["verb_class"].as_str().unwrap_or("lookup").into(),
        },
        covers: vec![],
        postconditions: vec![],
    }
}

fn proceeds(action: GateAction) -> bool {
    matches!(action, GateAction::Execute | GateAction::ExecuteFlagged)
}

#[test]
fn corpus_p4_gate_oracle() {
    let entries: Vec<Value> = serde_json::from_str(CORPUS).expect("parse corpus");
    let gate = AmbiguityAwareGate::default();
    let mut checked = 0usize;
    let mut mismatches = Vec::new();

    for entry in &entries {
        let tier = entry["true_tier"].as_u64().unwrap_or(2) as u8;
        if tier < 2 {
            continue;
        }
        let grounded = entry["grounded_label"].as_bool().unwrap_or(false);
        let art = corpus_entry_to_artifact(entry);
        let decision = gate.decide(&art, Tier::from_u8(tier));
        let ok = proceeds(decision.action) == grounded;
        if !ok {
            mismatches.push(format!(
                "{}: grounded={grounded} action={:?}",
                entry["id"].as_str().unwrap_or("?"),
                decision.action
            ));
        }
        checked += 1;
    }

    assert!(
        checked >= 75,
        "expected substantial corpus subset, got {checked}"
    );
    assert!(
        mismatches.is_empty(),
        "P4 oracle mismatches ({}): {:?}",
        mismatches.len(),
        mismatches.iter().take(5).collect::<Vec<_>>()
    );
}
