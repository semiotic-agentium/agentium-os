// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Fast-path gate activity reads for Trust operator rollups.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::{
    SurrealProvenanceStore, agent_runtime_index::normalize_agent_field_for_ops,
    payload::decode_payload_row,
};
use crate::{
    episode::AgentGateActivityFilters,
    error::Result,
    metamodel::{GraphQuery, SortDir, SortKey, labels},
    ops_types::ProvenanceOpsRow,
    store::GateActivityQueryResult,
    surreal_tables::{PAYLOAD_ROW_SELECT, TBL_PAYLOAD},
};

pub const GATE_ACTIVITY_MAX_ROWS: u64 = 5_000;

impl SurrealProvenanceStore {
    pub(super) async fn run_gate_activity_query(
        &self,
        filters: AgentGateActivityFilters,
    ) -> Result<GateActivityQueryResult> {
        let limit = filters.page_size.clamp(50, GATE_ACTIVITY_MAX_ROWS as u32) as u64;
        let fetch_limit = limit.saturating_add(1);

        // Registry table only — never graph-scan AgentRuntimeInstance on this hot path.
        let index = self.load_agent_runtime_index_from_registry().await?;
        let agent_package_filter = filters.agent_package.clone();

        let q = GraphQuery::<labels::ToolCall, _>::new()
            .all()
            .with_wall_time_range(
                Some(filters.from_timestamp_ms),
                Some(filters.to_timestamp_ms),
            )
            .with_recorded_gate_decision()
            .order_by(SortKey::ProvTime, SortDir::Asc)
            .paginate(0, fetch_limit);

        let (sql, binds) = q.into_surreal();
        let raw_rows = self.execute_typed_node_query(&sql, &binds).await?;
        let truncated = raw_rows.len() > limit as usize;
        let raw_rows: Vec<_> = raw_rows.into_iter().take(limit as usize).collect();

        let mut payload_ids = Vec::new();
        for row in &raw_rows {
            if let Some(props) = row.get("props").and_then(Value::as_object)
                && let Some(id) = props
                    .get("a2a_tool_call_payload_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            {
                payload_ids.push(id.to_string());
            }
        }
        let payload_map = self.batch_read_payloads(&payload_ids).await?;

        let identity_by_agent_id = index.identity_by_agent_id.clone();
        let mut ops_rows = Vec::with_capacity(raw_rows.len());
        for row in raw_rows {
            let Some(mut out) = canonicalize_gate_tool_row(&row) else {
                continue;
            };
            apply_gate_agent_identity(&mut out, &identity_by_agent_id);
            if let Some(ref pkg) = agent_package_filter
                && !gate_row_matches_agent_package(&out, pkg)
            {
                continue;
            }
            attach_gate_tool_args(&mut out, &payload_map);
            ops_rows.push(ProvenanceOpsRow::from_map(out));
        }

        Ok(GateActivityQueryResult {
            rows: ops_rows,
            truncated,
        })
    }

    async fn batch_read_payloads(&self, payload_ids: &[String]) -> Result<HashMap<String, String>> {
        if payload_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let unique: Vec<String> = payload_ids
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let in_list = unique
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE payload_id IN [{in_list}]"
        );
        let rows = self.query_sql_rows(&sql).await?;
        let decoded: Vec<_> = rows
            .into_iter()
            .map(decode_payload_row)
            .collect::<Result<Vec<_>>>()?;
        let hydrated = self.hydrate_payload_records(decoded).await?;
        Ok(hydrated
            .into_iter()
            .map(|rec| (rec.payload_id.clone(), rec.payload_json))
            .collect())
    }
}

fn canonicalize_gate_tool_row(row: &Value) -> Option<Map<String, Value>> {
    let props = row.get("props")?.as_object()?;
    let node_id = row
        .get("node_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut out = Map::new();
    out.insert(
        "activity_id".to_string(),
        Value::String(node_id.to_string()),
    );
    for (k, v) in props {
        out.insert(k.clone(), v.clone());
    }
    if let Some(v) = out.get("a2a_context_id").cloned() {
        out.insert("context_id".to_string(), v);
    }
    if let Some(v) = out.get("a2a_task_id").cloned() {
        out.insert("task_id".to_string(), v);
    }
    if let Some(v) = out.get("a2a_agent_id").cloned() {
        out.insert("agent_id".to_string(), v);
    }
    if let Some(v) = out.get("a2a_tool_name").cloned() {
        out.insert("tool_name".to_string(), v);
    }
    if let Some(gate) = out.get("a2a_gate").cloned() {
        out.insert("gate".to_string(), gate);
    }
    let timestamp_ms = out
        .get("prov_endTime")
        .and_then(Value::as_u64)
        .or_else(|| out.get("prov_startTime").and_then(Value::as_u64))
        .or_else(|| out.get("prov_time").and_then(Value::as_u64))
        .or_else(|| out.get("a2a_event_order").and_then(Value::as_u64))
        .unwrap_or(0);
    out.insert(
        "timestamp_ms".to_string(),
        Value::Number(timestamp_ms.into()),
    );
    let (activity_outcome, activity_status) = {
        let outcome = out
            .get("a2a_activity_outcome")
            .and_then(Value::as_str)
            .unwrap_or("Success");
        let status = out
            .get("a2a_activity_status")
            .and_then(Value::as_str)
            .unwrap_or("Completed");
        (outcome.to_string(), status.to_string())
    };
    out.insert(
        "activity_outcome".to_string(),
        Value::String(activity_outcome),
    );
    out.insert(
        "activity_status".to_string(),
        Value::String(activity_status),
    );
    Some(out)
}

fn apply_gate_agent_identity(
    row: &mut Map<String, Value>,
    identity_by_agent_id: &HashMap<String, (String, String)>,
) {
    let Some(agent_id) = row.get("agent_id").and_then(Value::as_str) else {
        if let Some(pkg) = row
            .get("a2a_agent_type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            row.insert("agent_package".to_string(), Value::String(pkg.to_string()));
        }
        return;
    };
    if let Some((agent_package, agent_version)) = identity_by_agent_id.get(agent_id) {
        row.insert(
            "agent_package".to_string(),
            Value::String(agent_package.clone()),
        );
        row.insert(
            "agent_version".to_string(),
            Value::String(agent_version.clone()),
        );
        row.insert(
            "agent_display".to_string(),
            Value::String(format!("{agent_package}/{agent_version}")),
        );
        return;
    }
    let agent_package =
        normalize_agent_field_for_ops(row.get("a2a_agent_type").and_then(Value::as_str), "unknown");
    row.insert("agent_package".to_string(), Value::String(agent_package));
}

fn gate_row_matches_agent_package(row: &Map<String, Value>, agent_package: &str) -> bool {
    row.get("agent_package")
        .and_then(Value::as_str)
        .is_some_and(|pkg| pkg == agent_package)
}

fn attach_gate_tool_args(row: &mut Map<String, Value>, payload_map: &HashMap<String, String>) {
    let args = if let Some(inline) = row.get("a2a_args").and_then(parse_inline_json) {
        inline
    } else if let Some(json) = row
        .get("a2a_tool_call_payload_id")
        .and_then(Value::as_str)
        .and_then(|id| payload_map.get(id))
    {
        serde_json::from_str::<Value>(json).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    row.insert("args".to_string(), args);
}

fn parse_inline_json(value: &Value) -> Option<Value> {
    match value {
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        Value::String(s) => serde_json::from_str(s).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn gate_row_matches_agent_package_compares_resolved_package() {
        let row = json!({"agent_package": "clickup-agent"})
            .as_object()
            .unwrap()
            .clone();
        assert!(gate_row_matches_agent_package(&row, "clickup-agent"));
        assert!(!gate_row_matches_agent_package(&row, "slack-agent"));
    }
}
