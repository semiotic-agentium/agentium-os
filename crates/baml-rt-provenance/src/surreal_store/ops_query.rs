//! [`ProvenanceOpsQuery`] and ops-row enrichment helpers.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use baml_rt_core::ids::ContextId;
use serde_json::{Map, Value};

use super::{
    SurrealProvenanceStore,
    helpers::{
        json_value_from_embedded_string, map_surreal_error, normalize_payload_text_query,
        parse_json_object_field, query_take_zero,
    },
    payload::{
        ParsedArchiveRef, archive_payload_from_record, archive_ref_for_activity,
        archive_ref_for_payload, parse_archive_ref,
    },
};
use crate::{
    error::{ProvenanceError, Result},
    id_semantics::context_entity_id_string,
    store::{
        ArchiveRef, ProvenanceArchiveRecord, ProvenanceOpsQuery, ProvenanceOpsQueryRequest,
        ProvenanceOpsQueryResponse, ProvenanceOpsResource, ProvenanceResponseProfile,
    },
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::{context_scope, semantic_labels},
};

impl SurrealProvenanceStore {
    /// Load agent identity map: agent_id -> (agent_package, agent_version).
    /// Queries AgentRuntimeInstance nodes for package/version metadata.
    async fn load_agent_identity_map(&self) -> Result<HashMap<String, (String, String)>> {
        let query = format!(
            "SELECT props.a2a_agent_id AS agent_id, \
                    props.a2a_agent_type AS agent_package, \
                    props.a2a_agent_version AS agent_version \
             FROM {TBL_NODE} WHERE label = 'AgentRuntimeInstance'"
        );
        let rows: Vec<Value> = self.query_sql_rows(&query).await?;

        let mut out: HashMap<String, (String, String)> = HashMap::new();
        for row in rows {
            let Some(agent_id) = row
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let agent_id = agent_id.to_string();
            let agent_package =
                normalize_agent_field(row.get("agent_package").and_then(Value::as_str), "unknown");
            let agent_version =
                normalize_agent_field(row.get("agent_version").and_then(Value::as_str), "unknown");
            out.insert(agent_id, (agent_package, agent_version));
        }
        Ok(out)
    }

    /// Load failure classification for the given activity node ids only (failed LLM/tool rows).
    /// Traverses `WAS_CLASSIFIED_BY` → `FailureClassification` entity; no global graph scan.
    async fn load_failure_classification_for_activity_ids(
        &self,
        activity_ids: &[String],
    ) -> Result<HashMap<String, (String, String)>> {
        if activity_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let classified_by = semantic_labels::WAS_CLASSIFIED_BY;
        let edge_query = format!(
            "SELECT from_id, to_id OMIT id FROM {TBL_EDGE} \
             WHERE rel_type = '{classified_by}' \
               AND from_id IN $ids \
               AND to_label = 'FailureClassification'"
        );
        let mut edge_response = self
            .db
            .query(&edge_query)
            .bind(("ids", activity_ids.to_vec()))
            .await
            .map_err(map_surreal_error)?;
        let edge_rows: Vec<Value> = query_take_zero(&mut edge_response, map_surreal_error)?;

        if edge_rows.is_empty() {
            return Ok(HashMap::new());
        }

        let fc_node_ids: Vec<String> = edge_rows
            .iter()
            .filter_map(|r| {
                r.get("to_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            })
            .collect::<HashSet<String>>()
            .into_iter()
            .collect();

        let fc_query = format!(
            "SELECT node_id, props.a2a_failure_class AS failure_class, props.a2a_failure_evidence AS failure_evidence \
             FROM {TBL_NODE} WHERE node_id IN $ids"
        );
        let mut fc_response = self
            .db
            .query(&fc_query)
            .bind(("ids", fc_node_ids))
            .await
            .map_err(map_surreal_error)?;
        let fc_rows: Vec<Value> = query_take_zero(&mut fc_response, map_surreal_error)?;

        let mut fc_map: HashMap<String, (String, String)> = HashMap::new();
        for row in fc_rows {
            let node_id = row
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if node_id.is_empty() {
                continue;
            }
            let class = normalize_agent_field(
                row.get("failure_class").and_then(Value::as_str),
                "failed_graph_incomplete",
            );
            let evidence = normalize_agent_field(
                row.get("failure_evidence").and_then(Value::as_str),
                "failed_graph_incomplete",
            );
            fc_map.insert(node_id, (class, evidence));
        }

        let mut out: HashMap<String, (String, String)> = HashMap::new();
        for row in edge_rows {
            let from_id = row
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let to_id = row
                .get("to_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            if from_id.is_empty() || from_id == "null" {
                continue;
            }

            if let Some((class, evidence)) = fc_map.get(&to_id) {
                if let Some(existing) = out.get(&from_id) {
                    let incoming = (class.clone(), evidence.clone());
                    if existing != &incoming {
                        return Err(ProvenanceError::InvalidEvent {
                            activity_anchor: from_id,
                            reason: format!(
                                "multiple conflicting failure classifications for activity: existing=({}, {}), incoming=({}, {})",
                                existing.0, existing.1, incoming.0, incoming.1
                            ),
                        });
                    }
                } else {
                    out.insert(from_id, (class.clone(), evidence.clone()));
                }
            }
        }
        Ok(out)
    }

    /// Aggregate LLM call durations by message_id for a context.
    /// Returns message_id -> total_llm_duration_ms map.
    async fn load_llm_duration_by_message(
        &self,
        context_id: &ContextId,
    ) -> Result<HashMap<String, u64>> {
        let ctx_node = context_entity_id_string(context_id.as_str());
        let scoped = context_scope::SCOPED_TO;
        let query = format!(
            "SELECT props.a2a_message_id AS message_id, props.a2a_duration_ms AS duration_ms \
             FROM {TBL_NODE} WHERE label = 'LlmCall' \
               AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
                 WHERE to_id = $ctx_node AND rel_type = '{scoped}' AND from_label = 'LlmCall') \
               AND props.a2a_duration_ms IS NOT NULL"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("ctx_node", ctx_node))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let mut out: HashMap<String, u64> = HashMap::new();
        for row in rows {
            let message_id = row
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if message_id.is_empty() {
                continue;
            }
            let duration = row.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
            *out.entry(message_id).or_insert(0) += duration;
        }
        Ok(out)
    }

    /// Aggregate tool call durations by message_id for a context.
    /// Returns message_id -> total_tool_duration_ms map.
    async fn load_tool_duration_by_message(
        &self,
        context_id: &ContextId,
    ) -> Result<HashMap<String, u64>> {
        let ctx_node = context_entity_id_string(context_id.as_str());
        let scoped = context_scope::SCOPED_TO;
        let query = format!(
            "SELECT props.a2a_message_id AS message_id, props.a2a_duration_ms AS duration_ms \
             FROM {TBL_NODE} WHERE label = 'ToolCall' \
               AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
                 WHERE to_id = $ctx_node AND rel_type = '{scoped}' AND from_label = 'ToolCall') \
               AND props.a2a_duration_ms IS NOT NULL"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("ctx_node", ctx_node))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let mut out: HashMap<String, u64> = HashMap::new();
        for row in rows {
            let message_id = row
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if message_id.is_empty() {
                continue;
            }
            let duration = row.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
            *out.entry(message_id).or_insert(0) += duration;
        }
        Ok(out)
    }
}

fn ops_row_timestamp_ms(row: &Map<String, Value>) -> u64 {
    row.get("timestamp_ms").and_then(Value::as_u64).unwrap_or(0)
}

fn ops_row_is_failed(row: &Map<String, Value>) -> bool {
    row.get("activity_outcome").and_then(Value::as_str) == Some("Failed")
}

fn ops_row_is_success(row: &Map<String, Value>) -> bool {
    row.get("activity_outcome").and_then(Value::as_str) == Some("Success")
}

// ---------------------------------------------------------------------------
// Ops query parameter validation
// ---------------------------------------------------------------------------

/// Valid field names for sort_by and group_by parameters.
/// Must match the OpsField::parse allowlist.
fn parse_ops_field(raw: &str) -> Option<&str> {
    match raw {
        "activity_id"
        | "activity_kind"
        | "timestamp_ms"
        | "duration_ms"
        | "total_tokens"
        | "prompt_tokens"
        | "completion_tokens"
        | "cached_input_tokens"
        | "agent_id"
        | "agent_display"
        | "agent_package"
        | "agent_version"
        | "context_id"
        | "task_id"
        | "message_id"
        | "provider"
        | "model"
        | "tool_name"
        | "baml_prompt"
        | "role"
        | "activity_outcome"
        | "activity_status"
        | "failure_class"
        | "failure_evidence"
        | "total_processing_ms"
        | "llm_duration_ms_sum"
        | "tool_duration_ms_sum" => Some(raw),
        _ => None,
    }
}

/// Validate and parse sort_by parameter. Defaults to "timestamp_ms".
fn parse_ops_sort_by(raw: Option<&str>) -> Result<&str> {
    let field = raw.unwrap_or("timestamp_ms");
    parse_ops_field(field).ok_or_else(|| ProvenanceError::InvalidEvent {
        activity_anchor: "ops_query".to_string(),
        reason: format!("unsupported sort field: {field}"),
    })
}

/// Validate and parse sort_dir parameter. Returns true if descending. Defaults to desc.
fn parse_ops_sort_dir(raw: Option<&str>) -> Result<bool> {
    match raw.unwrap_or("desc") {
        "asc" | "ASC" => Ok(false),
        "desc" | "DESC" => Ok(true),
        other => Err(ProvenanceError::InvalidEvent {
            activity_anchor: "ops_query".to_string(),
            reason: format!("unsupported sort direction: {other}"),
        }),
    }
}

/// Validate and parse group_by parameter. Defaults to ["agent_id"] if empty.
fn parse_ops_group_by(raw: &[String]) -> Result<Vec<String>> {
    if raw.is_empty() {
        return Ok(vec!["agent_id".to_string()]);
    }
    raw.iter()
        .map(|field| {
            parse_ops_field(field)
                .map(|f| f.to_string())
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    activity_anchor: "ops_query".to_string(),
                    reason: format!("unsupported group dimension: {field}"),
                })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Row enrichment helpers (finalize_call_row, apply_agent_identity_fields)
// ---------------------------------------------------------------------------

/// Normalize agent field: trim, filter empty/null strings, use fallback.
fn normalize_agent_field(raw: Option<&str>, fallback: &str) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty() && *s != "null")
        .unwrap_or(fallback)
        .to_string()
}

/// Parse a JSON-like string field from row props.
fn parse_json_field(row: &Map<String, Value>, field: &str) -> Option<Value> {
    row.get(field).and_then(parse_json_object_field)
}

/// Parse a JSON-like string into a Value (string fallback).
fn parse_json_like_string(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Apply agent identity fields (agent_package, agent_version, agent_display) from identity map.
fn apply_agent_identity_fields(
    row: &mut Map<String, Value>,
    identity_by_agent_id: &HashMap<String, (String, String)>,
) {
    let Some(agent_id) = row.get("agent_id").and_then(Value::as_str) else {
        return;
    };
    let Some((agent_package, agent_version)) = identity_by_agent_id.get(agent_id) else {
        return;
    };
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
}

/// Nest drift fields into a "drift" sub-object.
fn nest_llm_drift_fields(row: &mut Map<String, Value>) {
    let drift_citation = row.remove("drift_citation");
    let drift_score = row.remove("drift_score");
    let drift_severity = row.remove("drift_severity");
    let drift_mode = row.remove("drift_mode");
    let drift_warn_min_score = row.remove("drift_warn_min_score");
    let drift_block_min_score = row.remove("drift_block_min_score");
    let intent_text_preview = row.remove("intent_text_preview");
    let response_text_preview = row.remove("response_text_preview");
    let step_text_preview = row.remove("step_text_preview");

    let plan_intent = row.remove("plan_drift_intent_alignment");
    let plan_step = row.remove("plan_drift_step_alignment");
    let plan_traj = row.remove("plan_drift_trajectory");
    let plan_adherence = row.remove("plan_drift_adherence");
    let plan_severity = row.remove("plan_drift_composite_severity");

    let has_tactical = drift_score.is_some()
        || drift_severity.is_some()
        || drift_mode.is_some()
        || drift_warn_min_score.is_some()
        || drift_block_min_score.is_some()
        || intent_text_preview.is_some()
        || response_text_preview.is_some()
        || step_text_preview.is_some();

    let has_plan_drift = plan_intent.is_some()
        || plan_step.is_some()
        || plan_traj.is_some()
        || plan_adherence.is_some()
        || plan_severity.is_some();

    if !has_tactical && !has_plan_drift && drift_citation.is_none() {
        return;
    }

    let mut drift = Map::new();
    if let Some(value) = drift_score
        && !value.is_null()
    {
        drift.insert("score".to_string(), value);
    }
    if let Some(value) = drift_severity
        && !value.is_null()
    {
        drift.insert("severity".to_string(), value);
    }
    if let Some(value) = drift_mode
        && !value.is_null()
    {
        drift.insert("mode".to_string(), value);
    }
    if let Some(value) = drift_warn_min_score
        && !value.is_null()
    {
        drift.insert("warnMinScore".to_string(), value);
    }
    if let Some(value) = drift_block_min_score
        && !value.is_null()
    {
        drift.insert("blockMinScore".to_string(), value);
    }
    if let Some(value) = intent_text_preview
        && !value.is_null()
    {
        drift.insert("intentTextPreview".to_string(), value);
    }
    if let Some(value) = response_text_preview
        && !value.is_null()
    {
        drift.insert("responseTextPreview".to_string(), value);
    }
    if let Some(value) = step_text_preview
        && !value.is_null()
    {
        drift.insert("stepTextPreview".to_string(), value);
    }

    if let Some(value) = drift_citation
        && !value.is_null()
    {
        // Citation drift is stored as a JSON string by storage_safe_props (which stringifies
        // Value::Object). Parse it back so downstream consumers get a proper object, not a string.
        let parsed = json_value_from_embedded_string(&value);
        drift.insert("citation".to_string(), parsed);
    }

    // Nest plan drift fields into drift.plan sub-object.
    if has_plan_drift {
        let mut plan = Map::new();
        if let Some(v) = plan_intent
            && !v.is_null()
        {
            plan.insert("intentAlignment".to_string(), v);
        }
        if let Some(v) = plan_step
            && !v.is_null()
        {
            plan.insert("stepAlignment".to_string(), v);
        }
        if let Some(v) = plan_traj
            && !v.is_null()
        {
            plan.insert("trajectoryDrift".to_string(), v);
        }
        if let Some(v) = plan_adherence
            && !v.is_null()
        {
            plan.insert("planAdherenceScore".to_string(), v);
        }
        if let Some(v) = plan_severity
            && !v.is_null()
        {
            plan.insert("compositeSeverity".to_string(), v);
        }
        if !plan.is_empty() {
            drift.insert("plan".to_string(), Value::Object(plan));
        }
    }

    if !drift.is_empty() {
        row.insert("drift".to_string(), Value::Object(drift));
    }
}

fn percentile(sorted_values: &[f64], q: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_values.len() as f64 - 1.0) * q).round() as usize;
    sorted_values[idx.min(sorted_values.len() - 1)]
}

fn build_hotspot_groups(
    rows: &[Map<String, Value>],
    group_dims: &[String],
    top_k: usize,
) -> Vec<Value> {
    type HotspotAggregate = (Vec<Option<String>>, u64, u64, u64, u64);
    if rows.is_empty() {
        return Vec::new();
    }
    let mut groups: HashMap<String, HotspotAggregate> = HashMap::new();
    for row in rows {
        let group_values: Vec<Option<String>> = group_dims
            .iter()
            .map(|d| {
                row.get(d).and_then(|v| match v {
                    Value::Null => None,
                    Value::String(s) => {
                        let trimmed = s.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    }
                    _ => Some(v.to_string()),
                })
            })
            .collect();
        let key = serde_json::to_string(&group_values).unwrap_or_default();
        let duration = row.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
        let tokens = row.get("total_tokens").and_then(Value::as_u64).unwrap_or(0);
        let failed = u64::from(ops_row_is_failed(row));
        let entry = groups
            .entry(key)
            .or_insert_with(|| (group_values.clone(), 0, 0, 0, 0));
        entry.1 += 1;
        entry.2 += failed;
        entry.3 += duration;
        entry.4 += tokens;
    }

    let mut out: Vec<Value> = groups
        .into_iter()
        .map(
            |(_k, (group_values, count, failed, duration_sum, token_sum))| {
                let avg_duration = if count == 0 {
                    0.0
                } else {
                    duration_sum as f64 / count as f64
                };
                let avg_tokens = if count == 0 {
                    0.0
                } else {
                    token_sum as f64 / count as f64
                };
                let group_key = group_values
                    .iter()
                    .map(|v| v.clone().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("|");
                serde_json::json!({
                    "groupKey": group_key,
                    "groupValues": group_values,
                    "groupDimensions": group_dims,
                    "count": count,
                    "failed": failed,
                    "failureRate": if count == 0 { 0.0 } else { failed as f64 / count as f64 },
                    "avgDurationMs": avg_duration,
                    "avgTotalTokens": avg_tokens
                })
            },
        )
        .collect();
    out.sort_by(|a, b| {
        let ad = a
            .get("avgDurationMs")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let bd = b
            .get("avgDurationMs")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        bd.partial_cmp(&ad).unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(top_k);
    out
}

#[async_trait]
impl ProvenanceOpsQuery for SurrealProvenanceStore {
    async fn query_ops(
        &self,
        mut request: ProvenanceOpsQueryRequest,
    ) -> Result<ProvenanceOpsQueryResponse> {
        let profile = request
            .response_profile
            .clone()
            .unwrap_or(ProvenanceResponseProfile::UiFull);
        let page_cap = match profile {
            ProvenanceResponseProfile::UiFull => 200_u32,
            ProvenanceResponseProfile::ToolCompact => 50_u32,
        };
        let requested_page = request.page_size.unwrap_or(50).max(1);
        let page_size = requested_page.min(page_cap) as usize;
        let offset = request
            .cursor
            .as_deref()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        request.page_size = Some(page_size as u32);

        let compact_profile = matches!(profile, ProvenanceResponseProfile::ToolCompact);

        // Load enrichment maps for row post-processing.
        let identity_by_agent_id = self.load_agent_identity_map().await?;

        let label = match request.resource {
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => "LlmCall",
            ProvenanceOpsResource::ToolCalls => "ToolCall",
            ProvenanceOpsResource::Messages => "Message",
            ProvenanceOpsResource::LifecycleEvents => "AgentStop",
        };

        // Build WHERE clause with bind params only.
        let mut where_clauses = vec!["label = $label".to_string()];
        let mut binds: Vec<(String, Value)> =
            vec![("label".to_string(), Value::String(label.to_string()))];

        if let Some(ref ctx) = request.filters.context_id {
            let ctx_node = context_entity_id_string(ctx.as_str());
            let scoped = context_scope::SCOPED_TO;
            where_clauses.push(format!(
                "node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
                 WHERE to_id = $ctx_node AND rel_type = '{scoped}')"
            ));
            binds.push(("ctx_node".to_string(), Value::String(ctx_node)));
        }
        if !matches!(request.resource, ProvenanceOpsResource::Messages)
            && let Some(ref tid) = request.filters.task_id
        {
            let task_exec = crate::id_semantics::task_execution_activity_id_string(tid.as_str());
            let task_call = crate::vocabulary::a2a_relations::TASK_CALL;
            where_clauses.push(format!(
                "node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
                 WHERE from_id = $task_exec_node AND rel_type = '{task_call}')"
            ));
            binds.push(("task_exec_node".to_string(), Value::String(task_exec)));
        }
        if let Some(ref tool_name) = request.filters.tool_name {
            where_clauses.push("props.a2a_tool_name = $tool_name".to_string());
            binds.push((
                "tool_name".to_string(),
                Value::String(tool_name.to_string()),
            ));
        }
        if let Some(ref model) = request.filters.model {
            where_clauses.push("props.a2a_model = $model".to_string());
            binds.push(("model".to_string(), Value::String(model.to_string())));
        }
        if let Some(ref provider) = request.filters.provider {
            where_clauses.push("props.a2a_client = $provider".to_string());
            binds.push(("provider".to_string(), Value::String(provider.to_string())));
        }
        if let Some(ref agent_id) = request.filters.agent_id {
            where_clauses.push("props.a2a_agent_id = $agent_id".to_string());
            binds.push((
                "agent_id".to_string(),
                Value::String(agent_id.as_str().to_string()),
            ));
        }

        let query = format!(
            "SELECT node_id, props FROM {TBL_NODE} WHERE {}",
            where_clauses.join(" AND ")
        );
        let mut q = self.db.query(&query);
        for (k, v) in binds {
            q = q.bind((k, v));
        }
        let mut response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        // Canonicalize rows to the public ops shape.
        let mut ops_rows: Vec<Map<String, Value>> = rows
            .iter()
            .filter_map(|row| {
                let props = row.get("props")?.as_object()?;
                let mut out = Map::new();
                let node_id = row
                    .get("node_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
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
                if let Some(v) = out.get("a2a_message_id").cloned() {
                    out.insert("message_id".to_string(), v);
                }
                if let Some(v) = out.get("a2a_agent_id").cloned() {
                    out.insert("agent_id".to_string(), v);
                }
                if let Some(v) = out.get("a2a_client").cloned() {
                    out.insert("provider".to_string(), v);
                }
                if let Some(v) = out.get("a2a_model").cloned() {
                    out.insert("model".to_string(), v);
                }
                if let Some(v) = out.get("a2a_tool_name").cloned() {
                    out.insert("tool_name".to_string(), v);
                }
                // Use a2a_prompt_name (base logical prompt) for display if available,
                // falling back to a2a_function_name (full variant) for backward compat.
                let baml_prompt_val = out
                    .get("a2a_prompt_name")
                    .or_else(|| out.get("a2a_function_name"))
                    .cloned();
                if let Some(v) = baml_prompt_val {
                    out.insert("baml_prompt".to_string(), v);
                }
                if let Some(v) = out.get("a2a_duration_ms").cloned() {
                    out.insert("duration_ms".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_prompt_tokens").cloned() {
                    out.insert("prompt_tokens".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_completion_tokens").cloned() {
                    out.insert("completion_tokens".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_total_tokens").cloned() {
                    out.insert("total_tokens".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_cached_input_tokens").cloned() {
                    out.insert("cached_input_tokens".to_string(), v);
                }

                // Timestamp: prefer prov_endTime > prov_startTime > prov_time > event_order fallback.
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

                // Outcome / status: messages and lifecycle events are fire-and-forget
                // markers with no success/fail outcome — mark them Success/Completed so
                // the outcome-segment filter below lets them through. LLM/tool calls
                // derive from the a2a_activity_outcome property.
                let is_outcome_synthetic = matches!(
                    request.resource,
                    ProvenanceOpsResource::Messages | ProvenanceOpsResource::LifecycleEvents
                );
                let (activity_outcome, activity_status) = if is_outcome_synthetic {
                    ("Success".to_string(), "Completed".to_string())
                } else {
                    let outcome = out
                        .get("a2a_activity_outcome")
                        .and_then(Value::as_str)
                        .unwrap_or("InProgress")
                        .to_string();
                    let status = if matches!(outcome.as_str(), "Success" | "Failed") {
                        "Completed".to_string()
                    } else {
                        "InProgress".to_string()
                    };
                    let normalized = match outcome.as_str() {
                        "Success" => "Success".to_string(),
                        "Failed" => "Failed".to_string(),
                        _ => "Indeterminate".to_string(),
                    };
                    (normalized, status)
                };
                out.insert(
                    "activity_outcome".to_string(),
                    Value::String(activity_outcome),
                );
                out.insert(
                    "activity_status".to_string(),
                    Value::String(activity_status),
                );
                out.insert(
                    "activity_kind".to_string(),
                    Value::String(match request.resource {
                        ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => {
                            "llm_call".to_string()
                        }
                        ProvenanceOpsResource::ToolCalls => "tool_call".to_string(),
                        ProvenanceOpsResource::Messages => "message_turn".to_string(),
                        ProvenanceOpsResource::LifecycleEvents => "lifecycle_event".to_string(),
                    }),
                );
                Some(out)
            })
            .collect();

        // Load message duration aggregations (only for Messages resource with context_id filter).
        // Aggregate LLM/tool durations per message for the Messages resource.
        let (llm_duration_by_message, tool_duration_by_message) =
            if matches!(request.resource, ProvenanceOpsResource::Messages)
                && let Some(ref context_id) = request.filters.context_id
            {
                let llm_map = self.load_llm_duration_by_message(context_id).await?;
                let tool_map = self.load_tool_duration_by_message(context_id).await?;
                (llm_map, tool_map)
            } else {
                (HashMap::new(), HashMap::new())
            };

        let needs_failure_enrichment = matches!(
            request.resource,
            ProvenanceOpsResource::LlmCalls
                | ProvenanceOpsResource::ToolCalls
                | ProvenanceOpsResource::Aggregates
        );
        let failure_by_activity_id = if needs_failure_enrichment {
            let failed_ids: Vec<String> = ops_rows
                .iter()
                .filter(|row| ops_row_is_failed(row))
                .filter_map(|row| row.get("activity_id").and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            if failed_ids.is_empty() {
                HashMap::new()
            } else {
                self.load_failure_classification_for_activity_ids(&failed_ids)
                    .await?
            }
        } else {
            HashMap::new()
        };

        // Enrich rows with additional fields.
        // This adds: activity_ref, payload refs, structured payloads, agent identity, failure fields, drift nesting.
        for row in &mut ops_rows {
            let activity_id = row
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            // Add activity_ref for all row types
            if !activity_id.is_empty() {
                row.insert(
                    "activity_ref".to_string(),
                    Value::String(archive_ref_for_activity(&activity_id)),
                );
            }

            // Apply agent identity fields (agent_package, agent_version, agent_display)
            apply_agent_identity_fields(row, &identity_by_agent_id);

            match request.resource {
                ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => {
                    // Add LLM-specific enrichment
                    // Payload refs
                    if let Some(payload_id) =
                        row.get("a2a_llm_call_payload_id").and_then(Value::as_str)
                    {
                        row.insert(
                            "llm_call_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }
                    if let Some(payload_id) =
                        row.get("a2a_llm_result_payload_id").and_then(Value::as_str)
                    {
                        row.insert(
                            "llm_result_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }

                    // Structured llm_call (from payload or inline)
                    let llm_call = if compact_profile {
                        Value::Null
                    } else if let Some(payload_id) =
                        row.get("a2a_llm_call_payload_id").and_then(Value::as_str)
                    {
                        self.read_payload_by_id(payload_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|p| parse_json_like_string(&p.payload_json))
                            .or_else(|| parse_json_field(row, "a2a_prompt"))
                            .unwrap_or(Value::Null)
                    } else {
                        parse_json_field(row, "a2a_prompt").unwrap_or(Value::Null)
                    };
                    row.insert("llm_call".to_string(), llm_call);

                    // Structured llm_result (from payload or inline result/error)
                    let llm_result = if compact_profile {
                        Value::Null
                    } else if let Some(payload_id) =
                        row.get("a2a_llm_result_payload_id").and_then(Value::as_str)
                    {
                        self.read_payload_by_id(payload_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|p| parse_json_like_string(&p.payload_json))
                            .unwrap_or(Value::Null)
                    } else {
                        let result_value = parse_json_field(row, "a2a_result");
                        let error_value = parse_json_field(row, "a2a_error");
                        match (result_value, error_value) {
                            (Some(result), Some(error)) => serde_json::json!({
                                "result": result,
                                "error": error
                            }),
                            (Some(result), None) => result,
                            (None, Some(error)) => serde_json::json!({ "error": error }),
                            (None, None) => Value::Null,
                        }
                    };
                    row.insert("llm_result".to_string(), llm_result);

                    // Clean up raw fields
                    row.remove("a2a_result");
                    row.remove("a2a_error");
                    row.remove("a2a_llm_call_payload_id");
                    row.remove("a2a_llm_result_payload_id");

                    // Nest drift fields
                    // First, copy drift fields from a2a_ prefix to non-prefixed for nesting
                    if let Some(v) = row.get("a2a_drift_score").cloned() {
                        row.insert("drift_score".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_severity").cloned() {
                        row.insert("drift_severity".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_mode").cloned() {
                        row.insert("drift_mode".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_warn_min_score").cloned() {
                        row.insert("drift_warn_min_score".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_block_min_score").cloned() {
                        row.insert("drift_block_min_score".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_intent_text_preview").cloned() {
                        row.insert("intent_text_preview".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_response_text_preview").cloned() {
                        row.insert("response_text_preview".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_step_text_preview").cloned() {
                        row.insert("step_text_preview".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_citation_drift").cloned() {
                        row.insert("drift_citation".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_intent_alignment").cloned() {
                        row.insert("plan_drift_intent_alignment".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_step_alignment").cloned() {
                        row.insert("plan_drift_step_alignment".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_trajectory").cloned() {
                        row.insert("plan_drift_trajectory".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_adherence").cloned() {
                        row.insert("plan_drift_adherence".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_composite_severity").cloned() {
                        row.insert("plan_drift_composite_severity".to_string(), v);
                    }
                    nest_llm_drift_fields(row);

                    // Add failure classification for failed calls (graph edge only; hard-fail if missing)
                    if ops_row_is_failed(row) {
                        let resolved =
                            failure_by_activity_id.get(&activity_id).ok_or_else(|| {
                                ProvenanceError::InvalidEvent {
                                    activity_anchor: activity_id.clone(),
                                    reason: "missing WAS_CLASSIFIED_BY failure classification for failed llm_call"
                                        .to_string(),
                                }
                            })?;
                        row.insert(
                            "failure_class".to_string(),
                            Value::String(resolved.0.clone()),
                        );
                        row.insert(
                            "failure_evidence".to_string(),
                            Value::String(resolved.1.clone()),
                        );
                    }
                }
                ProvenanceOpsResource::ToolCalls => {
                    // Add Tool-specific enrichment
                    // Payload refs
                    if let Some(payload_id) =
                        row.get("a2a_tool_call_payload_id").and_then(Value::as_str)
                    {
                        row.insert(
                            "tool_call_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }
                    if let Some(payload_id) = row
                        .get("a2a_tool_result_payload_id")
                        .and_then(Value::as_str)
                    {
                        row.insert(
                            "tool_result_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }

                    // Structured tool_call (name, args, phase)
                    let tool_name = row
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let (tool_args, tool_phase) = if let Some(payload_id) =
                        row.get("a2a_tool_call_payload_id").and_then(Value::as_str)
                    {
                        if let Ok(Some(payload)) = self.read_payload_by_id(payload_id).await {
                            let parsed = parse_json_like_string(&payload.payload_json);
                            let args = parsed.get("args").cloned().unwrap_or(Value::Null);
                            let phase = parsed
                                .get("phase")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string);
                            (args, phase)
                        } else {
                            (
                                parse_json_field(row, "a2a_args").unwrap_or(Value::Null),
                                row.get("a2a_phase")
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|v| !v.is_empty())
                                    .map(str::to_string),
                            )
                        }
                    } else {
                        (
                            parse_json_field(row, "a2a_args").unwrap_or(Value::Null),
                            row.get("a2a_phase")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string),
                        )
                    };
                    row.insert(
                        "tool_call".to_string(),
                        serde_json::json!({
                            "name": tool_name,
                            "args": tool_args,
                            "phase": tool_phase
                        }),
                    );

                    // Structured tool_result (from payload or inline result/error)
                    let tool_result = if let Some(payload_id) = row
                        .get("a2a_tool_result_payload_id")
                        .and_then(Value::as_str)
                    {
                        self.read_payload_by_id(payload_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|p| parse_json_like_string(&p.payload_json))
                            .unwrap_or(Value::Null)
                    } else {
                        let result_value = parse_json_field(row, "a2a_result");
                        let error_value = parse_json_field(row, "a2a_error");
                        match (result_value, error_value) {
                            (Some(result), Some(error)) => serde_json::json!({
                                "result": result,
                                "error": error
                            }),
                            (Some(result), None) => result,
                            (None, Some(error)) => serde_json::json!({ "error": error }),
                            (None, None) => Value::Null,
                        }
                    };
                    row.insert("tool_result".to_string(), tool_result);

                    // Clean up raw fields
                    row.remove("a2a_args");
                    row.remove("a2a_phase");
                    row.remove("a2a_result");
                    row.remove("a2a_error");
                    row.remove("a2a_tool_call_payload_id");
                    row.remove("a2a_tool_result_payload_id");

                    // Add failure classification for failed calls (graph edge only; hard-fail if missing)
                    if ops_row_is_failed(row) {
                        let resolved =
                            failure_by_activity_id.get(&activity_id).ok_or_else(|| {
                                ProvenanceError::InvalidEvent {
                                activity_anchor: activity_id.clone(),
                                reason:
                                    "missing WAS_CLASSIFIED_BY failure classification for failed tool_call"
                                        .to_string(),
                            }
                            })?;
                        row.insert(
                            "failure_class".to_string(),
                            Value::String(resolved.0.clone()),
                        );
                        row.insert(
                            "failure_evidence".to_string(),
                            Value::String(resolved.1.clone()),
                        );
                    }
                }
                ProvenanceOpsResource::Messages => {
                    // Add Message-specific enrichment

                    // Get message_id for duration lookups
                    let message_id = row
                        .get("message_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();

                    // Add LLM/tool duration aggregates
                    let llm_sum = llm_duration_by_message
                        .get(&message_id)
                        .copied()
                        .unwrap_or(0);
                    let tool_sum = tool_duration_by_message
                        .get(&message_id)
                        .copied()
                        .unwrap_or(0);
                    row.insert(
                        "llm_duration_ms_sum".to_string(),
                        Value::Number(llm_sum.into()),
                    );
                    row.insert(
                        "tool_duration_ms_sum".to_string(),
                        Value::Number(tool_sum.into()),
                    );
                    row.insert(
                        "total_processing_ms".to_string(),
                        Value::Number((llm_sum + tool_sum).into()),
                    );
                    row.insert(
                        "duration_ms".to_string(),
                        Value::Number((llm_sum + tool_sum).into()),
                    );

                    // Parse message content and extract text
                    let message_content =
                        parse_json_field(row, "a2a_content").unwrap_or(Value::Array(vec![]));
                    let message_text = match &message_content {
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n"),
                        Value::String(s) => s.clone(),
                        _ => String::new(),
                    };
                    row.insert("message_content".to_string(), message_content);
                    if !message_text.is_empty() {
                        row.insert("message_text".to_string(), Value::String(message_text));
                    }
                    row.remove("a2a_content");

                    // Add role and direction fields
                    if let Some(v) = row.get("a2a_role").cloned() {
                        row.insert("role".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_direction").cloned() {
                        row.insert("direction".to_string(), v);
                    }
                }
                ProvenanceOpsResource::LifecycleEvents => {
                    // Lifecycle events (AgentStop) have minimal enrichment — just
                    // surface the stop_reason so callers can assert on it.
                }
            }
        }

        // Exclude non-terminal "open" phase tool rows from ToolCalls responses.
        if matches!(request.resource, ProvenanceOpsResource::ToolCalls) {
            ops_rows.retain(|row| {
                row.get("tool_call")
                    .and_then(|v| v.get("phase"))
                    .and_then(Value::as_str)
                    != Some("open")
            });
        }

        // Payload text filter: resolve matching activity_ids via FTS, then filter rows.
        // Payload text filtering applies to LlmCalls/ToolCalls/Aggregates only,
        // NOT for Messages (query_message_rows has no payload_text logic).
        // Empty/whitespace-only payload_text is treated as "no filter" (None),
        // not as "filter to empty set" which would return zero rows.
        let payload_text_activity_filter: Option<HashSet<String>> =
            if !matches!(request.resource, ProvenanceOpsResource::Messages)
                && let Some(ref payload_text) = request.filters.payload_text
            {
                // Check if normalized query would be empty - if so, treat as no filter
                let normalized = normalize_payload_text_query(payload_text);
                if normalized.is_empty() {
                    None
                } else {
                    let matching = self.search_payload_activity_ids(payload_text).await?;
                    Some(matching.into_iter().collect())
                }
            } else {
                None
            };

        // Rust-side common filters.
        let prompt_filter_lc = request
            .filters
            .baml_prompt
            .as_ref()
            .map(|prompt| prompt.to_ascii_lowercase());
        let outcome_segment = request
            .outcome
            .clone()
            .unwrap_or(crate::store::ProvenanceOutcomeSegment::Both);
        ops_rows.retain(|row| {
            if let Some(from_ms) = request.filters.from_timestamp_ms
                && ops_row_timestamp_ms(row) < from_ms
            {
                return false;
            }
            if let Some(to_ms) = request.filters.to_timestamp_ms
                && ops_row_timestamp_ms(row) > to_ms
            {
                return false;
            }
            if let Some(prompt_lc) = prompt_filter_lc.as_ref() {
                let prompt_value = row
                    .get("baml_prompt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !prompt_value.contains(prompt_lc) {
                    return false;
                }
            }
            if let Some(ref allowed) = payload_text_activity_filter {
                let activity_id = row
                    .get("activity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !allowed.contains(activity_id) {
                    return false;
                }
            }
            match outcome_segment {
                crate::store::ProvenanceOutcomeSegment::FailedOnly => ops_row_is_failed(row),
                crate::store::ProvenanceOutcomeSegment::SuccessfulOnly => ops_row_is_success(row),
                crate::store::ProvenanceOutcomeSegment::Both => {
                    ops_row_is_success(row) || ops_row_is_failed(row)
                }
            }
        });

        // Validate and apply sort parameters.
        let sort_by = parse_ops_sort_by(request.sort_by.as_deref())?;
        let sort_desc = parse_ops_sort_dir(request.sort_dir.as_deref())?;
        ops_rows.sort_by(|a, b| {
            let av = a.get(sort_by).cloned().unwrap_or(Value::Null);
            let bv = b.get(sort_by).cloned().unwrap_or(Value::Null);
            let ord = match (&av, &bv) {
                (Value::Number(an), Value::Number(bn)) => an
                    .as_f64()
                    .partial_cmp(&bn.as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal),
                (Value::String(as_), Value::String(bs_)) => as_.cmp(bs_),
                _ => std::cmp::Ordering::Equal,
            };
            let ord = if ord == std::cmp::Ordering::Equal {
                let aid = a
                    .get("activity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let bid = b
                    .get("activity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                aid.cmp(bid)
            } else {
                ord
            };
            if sort_desc { ord.reverse() } else { ord }
        });

        let mut durations: Vec<f64> = ops_rows
            .iter()
            .filter_map(|r| r.get("duration_ms").and_then(Value::as_f64))
            .collect();
        durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut tokens: Vec<f64> = ops_rows
            .iter()
            .filter_map(|r| r.get("total_tokens").and_then(Value::as_f64))
            .collect();
        tokens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let duration_p95 = percentile(&durations, 0.95);
        let duration_p99 = percentile(&durations, 0.99);
        let token_p95 = percentile(&tokens, 0.95);
        let token_p99 = percentile(&tokens, 0.99);

        let total_rows = ops_rows.len();
        let page_end = std::cmp::min(offset.saturating_add(page_size), total_rows);
        let page_rows = if offset < total_rows {
            ops_rows[offset..page_end].to_vec()
        } else {
            Vec::new()
        };
        let next_cursor = if page_end < total_rows {
            Some(page_end.to_string())
        } else {
            None
        };

        let top_k = request.top_k.unwrap_or(10) as usize;
        // Validate group_by.
        let effective_group_by = parse_ops_group_by(&request.group_by)?;
        let hotspot_groups = build_hotspot_groups(&ops_rows, &effective_group_by, top_k);
        let failed_count = ops_rows.iter().filter(|r| ops_row_is_failed(r)).count();
        let total_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| r.get("total_tokens").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        let prompt_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| r.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        let completion_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| {
                r.get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .sum();
        let cached_input_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| {
                r.get("cached_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .sum();
        let total_duration_sum: u64 = ops_rows
            .iter()
            .map(|r| r.get("duration_ms").and_then(Value::as_u64).unwrap_or(0))
            .sum();

        let mut summary = serde_json::json!({
            "count": total_rows,
            "failedCount": failed_count,
            "durationMsTotal": total_duration_sum,
            "totalTokens": total_tokens_sum,
            "promptTokensTotal": prompt_tokens_sum,
            "completionTokensTotal": completion_tokens_sum,
            "latencyHotspots": {
                "p95": duration_p95,
                "p99": duration_p99
            },
            "tokenHotspots": {
                "p95": token_p95,
                "p99": token_p99
            }
        });
        if matches!(
            request.resource,
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates
        ) && let Some(obj) = summary.as_object_mut()
        {
            obj.insert(
                "cachedInputTokensTotal".to_string(),
                Value::from(cached_input_tokens_sum),
            );
        }

        Ok(ProvenanceOpsQueryResponse {
            resource: request.resource,
            rows: page_rows.into_iter().map(Value::Object).collect(),
            summary,
            hotspot_groups,
            next_cursor,
            truncated: total_rows > page_size || requested_page > page_cap,
            applied_caps: Map::from_iter([
                (
                    "page_size".to_string(),
                    Value::Number((page_size as u64).into()),
                ),
                (
                    "max_page_size".to_string(),
                    Value::Number((page_cap as u64).into()),
                ),
                (
                    "top_k".to_string(),
                    Value::Number((request.top_k.unwrap_or(10) as u64).into()),
                ),
            ]),
        })
    }

    async fn resolve_archive_ref(
        &self,
        archive_ref: &str,
    ) -> Result<Option<ProvenanceArchiveRecord>> {
        let Some(parsed) = parse_archive_ref(archive_ref) else {
            return Ok(None);
        };
        match parsed {
            ParsedArchiveRef::PayloadId(payload_id) => {
                let Some(payload) = self.read_payload_by_id(payload_id).await? else {
                    return Ok(None);
                };
                Ok(Some(ProvenanceArchiveRecord {
                    archive_ref: ArchiveRef(archive_ref.to_string()),
                    payloads: vec![archive_payload_from_record(payload)?],
                }))
            }
            ParsedArchiveRef::ActivityId(activity_id) => {
                let payloads = self.read_payloads_by_activity(activity_id).await?;
                if payloads.is_empty() {
                    return Ok(None);
                }
                Ok(Some(ProvenanceArchiveRecord {
                    archive_ref: ArchiveRef(archive_ref.to_string()),
                    payloads: payloads
                        .into_iter()
                        .map(archive_payload_from_record)
                        .collect::<Result<Vec<_>>>()?,
                }))
            }
        }
    }
}
