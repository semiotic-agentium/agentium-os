// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Fleet- and agent-scoped gate activity aggregation for operator Settings surfaces.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::gate_row::{
    DeniedSnapshot, parse_gate_tool_row, resolve_telemetry_verdict, sort_gate_rows_chronologically,
};
use crate::{error::Result, ops_types::ProvenanceOpsRow, store::ProvenanceOpsQuery};

#[derive(Debug, Clone)]
pub struct AgentGateActivityFilters {
    pub agent_package: Option<String>,
    pub from_timestamp_ms: u64,
    pub to_timestamp_ms: u64,
    pub incident_limit: usize,
    pub page_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateIncidentRow {
    pub occurred_at_ms: u64,
    pub context_id: String,
    pub task_id: String,
    pub tool_name: String,
    pub tier: u8,
    pub decision: String,
    pub reason_code: String,
    pub deficient_nodes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_verdict: Option<String>,
    pub severity: String,
    pub tool_call_anchor: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGateCounts {
    pub deny: u32,
    pub ask: u32,
    pub pass_gated: u32,
    pub pass: u32,
    pub friction_denial: u32,
    pub prevented_error: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedCount {
    pub code: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGateActivity {
    pub agent_package: String,
    pub counts: AgentGateCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prevention_ratio: Option<f32>,
    pub top_reason_codes: Vec<RankedCount>,
    pub top_deficient_nodes: Vec<RankedCount>,
    pub recent_incidents: Vec<GateIncidentRow>,
}

#[derive(Debug, Clone, Default)]
struct AgentAccumulator {
    counts: AgentGateCounts,
    reason_codes: HashMap<String, u32>,
    deficient_nodes: HashMap<String, u32>,
    incidents: Vec<GateIncidentRow>,
}

impl AgentAccumulator {
    fn prevention_ratio(&self) -> Option<f32> {
        let prevented = self.counts.prevented_error;
        let friction = self.counts.friction_denial;
        let denom = prevented + friction;
        if denom == 0 {
            return None;
        }
        Some(prevented as f32 / denom as f32)
    }

    fn into_activity(self, agent_package: String, incident_limit: usize) -> AgentGateActivity {
        let prevention_ratio = self.prevention_ratio();
        let mut incidents = self.incidents;
        incidents.sort_by_key(|b| std::cmp::Reverse(b.occurred_at_ms));
        incidents.truncate(incident_limit);

        AgentGateActivity {
            agent_package,
            counts: self.counts,
            prevention_ratio,
            top_reason_codes: top_n(&self.reason_codes, 5),
            top_deficient_nodes: top_n(&self.deficient_nodes, 5),
            recent_incidents: incidents,
        }
    }
}

fn top_n(map: &HashMap<String, u32>, n: usize) -> Vec<RankedCount> {
    let mut items: Vec<_> = map
        .iter()
        .map(|(code, count)| RankedCount {
            code: code.clone(),
            count: *count,
        })
        .collect();
    items.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.code.cmp(&b.code)));
    items.truncate(n);
    items
}

fn incident_severity(decision: &str) -> &'static str {
    match decision {
        "deny" => "critical",
        "ask" => "warning",
        _ => "info",
    }
}

fn is_incident_row(decision: &str, telemetry_verdict: Option<&str>) -> bool {
    matches!(decision, "deny" | "ask")
        || matches!(
            telemetry_verdict,
            Some("friction_denial") | Some("prevented_error")
        )
}

pub fn agent_has_gate_activity(activity: &AgentGateActivity) -> bool {
    activity.counts.deny > 0
        || activity.counts.ask > 0
        || activity.counts.friction_denial > 0
        || activity.counts.prevented_error > 0
        || !activity.recent_incidents.is_empty()
}

pub async fn aggregate_agent_gate_activity(
    store: &dyn ProvenanceOpsQuery,
    filters: AgentGateActivityFilters,
) -> Result<(HashMap<String, AgentGateActivity>, bool)> {
    let incident_limit = filters.incident_limit;
    let result = store.query_gate_activity(filters).await?;
    let activity = aggregate_agent_gate_activity_from_rows(&result.rows, incident_limit);
    Ok((activity, result.truncated))
}

/// Aggregate gate activity from pre-fetched ops rows (for API adapters).
pub fn aggregate_agent_gate_activity_from_rows(
    rows: &[ProvenanceOpsRow],
    incident_limit: usize,
) -> HashMap<String, AgentGateActivity> {
    let mut sorted = rows.to_vec();
    sort_gate_rows_chronologically(&mut sorted);

    let mut by_agent: HashMap<String, AgentAccumulator> = HashMap::new();
    let mut recent_denies: HashMap<String, Vec<DeniedSnapshot>> = HashMap::new();

    for row in &sorted {
        process_row(row, &mut by_agent, &mut recent_denies);
    }

    by_agent
        .into_iter()
        .map(|(pkg, acc)| {
            let activity = acc.into_activity(pkg.clone(), incident_limit);
            (pkg, activity)
        })
        .collect()
}

fn process_row(
    row: &ProvenanceOpsRow,
    by_agent: &mut HashMap<String, AgentAccumulator>,
    recent_denies: &mut HashMap<String, Vec<DeniedSnapshot>>,
) {
    let Some(parsed) = parse_gate_tool_row(row) else {
        return;
    };
    let agent_package = parsed.agent_package.clone();
    let decision = parsed.decision.clone();

    let acc = by_agent.entry(agent_package.clone()).or_default();
    let denies = recent_denies.entry(agent_package).or_default();

    let (telemetry_verdict, friction, prevented) = resolve_telemetry_verdict(&parsed, denies);
    if friction {
        acc.counts.friction_denial += 1;
    }
    if prevented {
        acc.counts.prevented_error += 1;
    }

    match decision.as_str() {
        "deny" => {
            acc.counts.deny += 1;
            denies.push(DeniedSnapshot {
                tool_name: parsed.tool_name.clone(),
                args: parsed.args.clone(),
            });
        }
        "ask" => acc.counts.ask += 1,
        "pass_gated" => acc.counts.pass_gated += 1,
        "pass" => acc.counts.pass += 1,
        _ => {}
    }

    if !parsed.reason_code.is_empty() && matches!(decision.as_str(), "deny" | "ask") {
        *acc.reason_codes
            .entry(parsed.reason_code.clone())
            .or_insert(0) += 1;
    }
    for node in &parsed.deficient_nodes {
        *acc.deficient_nodes.entry(node.clone()).or_insert(0) += 1;
    }

    let severity = incident_severity(&decision).to_string();
    if is_incident_row(&decision, telemetry_verdict.as_deref()) {
        acc.incidents.push(GateIncidentRow {
            occurred_at_ms: parsed.occurred_at_ms,
            context_id: parsed.context_id,
            task_id: parsed.task_id,
            tool_name: parsed.tool_name,
            tier: parsed.tier,
            decision,
            reason_code: parsed.reason_code,
            deficient_nodes: parsed.deficient_nodes,
            telemetry_verdict,
            severity,
            tool_call_anchor: parsed.tool_call_anchor,
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn incident_severity_maps_deny_to_critical() {
        assert_eq!(incident_severity("deny"), "critical");
        assert_eq!(incident_severity("ask"), "warning");
    }

    #[test]
    fn process_row_groups_by_agent_package() {
        let mut by_agent = HashMap::new();
        let mut denies = HashMap::new();
        let row = ProvenanceOpsRow::from_map(
            json!({
                "agent_package": "slack-agent",
                "context_id": "ctx-1",
                "task_id": "task-1",
                "timestamp_ms": 1000,
                "a2a_activity_anchor": "act-1",
                "gate": {
                    "decision": "deny",
                    "toolName": "support/delete",
                    "tier": 3,
                    "reasonCode": "missing_postcondition",
                    "deficientNodes": ["ACTION"]
                }
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        process_row(&row, &mut by_agent, &mut denies);
        let activity = by_agent
            .remove("slack-agent")
            .expect("slack-agent")
            .into_activity("slack-agent".to_string(), 20);
        assert_eq!(activity.counts.deny, 1);
        assert_eq!(activity.recent_incidents.len(), 1);
        assert_eq!(activity.recent_incidents[0].severity, "critical");
    }

    #[test]
    fn friction_denial_requires_chronological_deny_before_pass() {
        let deny = ProvenanceOpsRow::from_map(
            json!({
                "agent_package": "pkg-a",
                "context_id": "ctx-1",
                "task_id": "task-1",
                "timestamp_ms": 1000,
                "gate": {
                    "decision": "deny",
                    "toolName": "support/delete",
                    "tier": 3,
                    "reasonCode": "missing_postcondition"
                },
                "args": {"command": "rm -rf /tmp/x"}
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let pass = ProvenanceOpsRow::from_map(
            json!({
                "agent_package": "pkg-a",
                "context_id": "ctx-1",
                "task_id": "task-1",
                "timestamp_ms": 2000,
                "gate": {
                    "decision": "pass",
                    "toolName": "support/delete",
                    "tier": 3
                },
                "args": {"command": "rm -rf /tmp/x"}
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let activity = aggregate_agent_gate_activity_from_rows(&[deny, pass], 20);
        let pkg = activity.get("pkg-a").expect("pkg-a");
        assert_eq!(pkg.counts.friction_denial, 1);
        assert_eq!(pkg.counts.prevented_error, 0);
    }
}
