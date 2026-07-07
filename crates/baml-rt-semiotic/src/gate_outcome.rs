// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Last gate evaluation per runtime scope — stamped into tool effect metadata.

use std::{collections::HashMap, sync::RwLock};

use baml_rt_core::context::RuntimeScope;
use serde::{Deserialize, Serialize};

use crate::telemetry::GateReasonCode;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateOutcome {
    pub tool_name: String,
    pub tier: u8,
    pub decision: String,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deficient_nodes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postcondition_passed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_authorization: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secs_since_denial: Option<u32>,
}

impl GateOutcome {
    pub fn new(
        tool_name: &str,
        tier: u8,
        decision: &str,
        reason: GateReasonCode,
        deficient_nodes: Vec<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            tier,
            decision: decision.to_string(),
            reason_code: reason.as_str().to_string(),
            deficient_nodes,
            postcondition_passed: None,
            gate_authorization: None,
            telemetry_verdict: None,
            secs_since_denial: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct GateOutcomeStore {
    inner: RwLock<HashMap<String, GateOutcome>>,
}

impl GateOutcomeStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(scope: &RuntimeScope) -> String {
        format!(
            "{}:{}",
            scope.agent_id().as_str(),
            scope.task_id_opt().map(|t| t.as_str()).unwrap_or("")
        )
    }

    pub fn record(&self, scope: &RuntimeScope, outcome: GateOutcome) {
        let key = Self::key(scope);
        let mut g = self.inner.write().expect("gate outcome lock");
        g.insert(key, outcome);
    }

    pub fn take(&self, scope: &RuntimeScope) -> Option<GateOutcome> {
        let key = Self::key(scope);
        let mut g = self.inner.write().expect("gate outcome lock");
        g.remove(&key)
    }
}
