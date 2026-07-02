// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Eval manifest parsing (`eval/cases.toml`).

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EvalManifest {
    #[serde(default)]
    pub defaults: EvalDefaults,
    #[serde(default)]
    pub case: Vec<EvalCase>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EvalDefaults {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub client: Option<String>,
    pub context_id: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EvalCase {
    pub id: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Single-turn sugar
    pub input: Option<String>,
    pub send: Option<String>,
    pub model: Option<String>,
    pub client: Option<String>,
    pub fixture: Option<String>,
    #[serde(default)]
    pub turns: Vec<EvalTurn>,
    #[serde(default)]
    pub assert: TurnAssert,
    #[serde(default, rename = "flow_assert")]
    pub flow_assert: FlowAssert,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EvalTurn {
    pub send: Option<String>,
    pub fixture: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub assert: TurnAssert,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TurnAssert {
    pub contains: Option<Vec<String>>,
    #[serde(rename = "not_contains")]
    pub not_contains: Option<Vec<String>>,
    pub task_states: Option<Vec<String>>,
    #[serde(rename = "not_task_states")]
    pub not_task_states: Option<Vec<String>>,
    pub artifact: Option<bool>,
    #[serde(rename = "max_llm_calls")]
    pub max_llm_calls: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FlowAssert {
    #[serde(rename = "max_turns")]
    pub max_turns: Option<u32>,
    #[serde(rename = "max_llm_calls")]
    pub max_llm_calls: Option<u32>,
    #[serde(rename = "tools_used")]
    pub tools_used: Option<Vec<String>>,
    #[serde(rename = "tools_forbidden")]
    pub tools_forbidden: Option<Vec<String>>,
}

fn default_mode() -> String {
    "chat".to_string()
}

impl EvalCase {
    pub fn resolved_turns(&self) -> Vec<EvalTurn> {
        if !self.turns.is_empty() {
            return self.turns.clone();
        }
        let text = self
            .send
            .clone()
            .or_else(|| self.input.clone())
            .unwrap_or_default();
        vec![EvalTurn {
            send: Some(text),
            fixture: self.fixture.clone(),
            mode: Some(self.mode.clone()),
            model: self.model.clone(),
            assert: self.assert.clone(),
        }]
    }
}

pub fn load_manifest(path: &std::path::Path) -> Result<EvalManifest> {
    let raw = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&raw)?)
}
