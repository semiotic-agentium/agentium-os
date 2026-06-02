// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! [`ProvenanceOpsQuery`] and ops-row enrichment helpers.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use baml_rt_conversation::view::ToolSessionPhase;
use baml_rt_core::ids::ContextId;
use serde_json::{Map, Value};

use super::{
    SurrealProvenanceStore,
    agent_runtime_index::{
        TaskAgentPackageCheck, normalize_agent_field_for_ops, task_agent_package_check,
    },
    helpers::{
        json_value_from_embedded_string, normalize_payload_text_query, parse_json_object_field,
    },
    payload::{
        ParsedArchiveRef, archive_payload_from_record, archive_ref_for_activity,
        archive_ref_for_payload, parse_archive_ref,
    },
};
use crate::{
    error::{ProvenanceError, Result},
    metamodel::{
        AgentRuntimeInstanceNodeId, ContextNodeId, EdgeProjection, FilterOp, GraphQuery,
        ScopeState, SemanticEdge, TaskExecutionNodeId, TaskNodeId, keys, labels,
    },
    observation::ops::build_ops_summary,
    ops_types::{ProvenanceOpsAppliedCaps, ProvenanceOpsHotspotGroup, ProvenanceOpsRow},
    store::{
        ArchiveRef, ProvenanceArchiveRecord, ProvenanceOpsFilters, ProvenanceOpsQuery,
        ProvenanceOpsQueryRequest, ProvenanceOpsQueryResponse, ProvenanceOpsResource,
        ProvenanceResponseProfile,
    },
};

impl SurrealProvenanceStore {
    /// Load failure classification for the given activity node ids only
    /// (failed LLM/tool rows). Traverses `WAS_CLASSIFIED_BY` →
    /// `FailureClassification` entity via the typed [`EdgeProjection`]
    /// surface; no global graph scan and no raw edge-label string
    /// interpolation.
    pub(super) async fn load_failure_classification_for_activity_ids(
        &self,
        activity_ids: &[String],
    ) -> Result<HashMap<String, (String, String)>> {
        if activity_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let (edge_sql, edge_binds) = EdgeProjection::for_edge(SemanticEdge::WasClassifiedBy)
            .from_id_in(activity_ids)
            .with_to_label::<labels::FailureClassification>()
            .into_surreal();
        let edge_rows = self
            .execute_typed_node_query(&edge_sql, &edge_binds)
            .await?;

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

        let (fc_sql, fc_binds) = GraphQuery::<labels::FailureClassification, _>::new()
            .all()
            .by_node_ids(&fc_node_ids)
            .into_surreal();
        let fc_rows = self.execute_typed_node_query(&fc_sql, &fc_binds).await?;

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
            let props = row.get("props").and_then(Value::as_object);
            let class = normalize_agent_field_for_ops(
                props
                    .and_then(|p| p.get("a2a_failure_class"))
                    .and_then(Value::as_str),
                "failed_graph_incomplete",
            );
            let evidence = normalize_agent_field_for_ops(
                props
                    .and_then(|p| p.get("a2a_failure_evidence"))
                    .and_then(Value::as_str),
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

    /// Aggregate LLM call durations by message_id for a context. Routed
    /// through the typed [`GraphQuery`] surface — the SCOPED_TO traversal
    /// is sourced from [`crate::metamodel::query::ScopedToContext`] and
    /// not stitched in via raw `context_scope::SCOPED_TO` interpolation.
    async fn load_llm_duration_by_message(
        &self,
        context_id: &ContextId,
    ) -> Result<HashMap<String, u64>> {
        let ctx_node = ContextNodeId::for_context_id(context_id);
        let (sql, binds) = GraphQuery::<labels::LlmCall, _>::new()
            .scoped_to_ctx(ctx_node)
            .into_surreal();
        let rows = self.execute_typed_node_query(&sql, &binds).await?;
        Ok(aggregate_duration_by_message(&rows))
    }

    /// Aggregate tool call durations by message_id for a context.
    async fn load_tool_duration_by_message(
        &self,
        context_id: &ContextId,
    ) -> Result<HashMap<String, u64>> {
        let ctx_node = ContextNodeId::for_context_id(context_id);
        let (sql, binds) = GraphQuery::<labels::ToolCall, _>::new()
            .scoped_to_ctx(ctx_node)
            .into_surreal();
        let rows = self.execute_typed_node_query(&sql, &binds).await?;
        Ok(aggregate_duration_by_message(&rows))
    }
}

/// Sum `props.a2a_duration_ms` per `props.a2a_message_id` from raw
/// `SELECT *` rows produced by the typed `GraphQuery`. Rows without a
/// message id or duration field are silently skipped (the typed query
/// fetches all scoped rows; missing-duration filtering is post-processed
/// here rather than in SQL).
fn aggregate_duration_by_message(rows: &[Value]) -> HashMap<String, u64> {
    let mut out: HashMap<String, u64> = HashMap::new();
    for row in rows {
        let Some(props) = row.get("props").and_then(Value::as_object) else {
            continue;
        };
        let Some(message_id) = props
            .get("a2a_message_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let duration = props
            .get("a2a_duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        *out.entry(message_id.to_string()).or_insert(0) += duration;
    }
    out
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

/// Tool rows whose graph `a2a_activity_outcome` was missing or not yet terminal are surfaced as
/// `Indeterminate` (see outcome mapping above). `Both` historically kept only Success/Failed,
/// which hid every in-flight or partially-persisted tool call from ops consumers.
fn ops_row_is_indeterminate(row: &Map<String, Value>) -> bool {
    row.get("activity_outcome").and_then(Value::as_str) == Some("Indeterminate")
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

/// Parse a JSON-like string field from row props.
fn parse_json_field(row: &Map<String, Value>, field: &str) -> Option<Value> {
    row.get(field).and_then(parse_json_object_field)
}

/// Parse a JSON-like string into a Value (string fallback).
fn parse_json_like_string(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// `tool_call` object for ops API responses: `phase` is always a string label, never JSON null.
///
/// Instances exist only after routing payload / row hints through [`ToolSessionPhase`].
struct OpsToolCallEnrichment {
    name: String,
    args: Value,
    phase: ToolSessionPhase,
}

impl OpsToolCallEnrichment {
    fn into_json_value(self) -> Value {
        serde_json::json!({
            "name": self.name,
            "args": self.args,
            "phase": self.phase.label(),
        })
    }
}

fn resolve_tool_session_phase_for_ops_row(
    parsed_tool_call_json: Option<&Value>,
    row: &Map<String, Value>,
) -> ToolSessionPhase {
    let phase_str = parsed_tool_call_json
        .and_then(|v| v.get("phase"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            row.get("a2a_phase")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
    let meta = match phase_str {
        Some(p) => serde_json::json!({ "phase": p }),
        None => serde_json::json!({}),
    };
    ToolSessionPhase::from_metadata(&meta)
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
) -> Vec<ProvenanceOpsHotspotGroup> {
    use crate::ops_types::ProvenanceOpsHotspotGroup;

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

    let mut out: Vec<ProvenanceOpsHotspotGroup> = groups
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
                ProvenanceOpsHotspotGroup {
                    group_key,
                    group_values,
                    group_dimensions: group_dims.to_vec(),
                    count,
                    failed,
                    failure_rate: if count == 0 {
                        0.0
                    } else {
                        failed as f64 / count as f64
                    },
                    avg_duration_ms: avg_duration,
                    avg_total_tokens: avg_tokens,
                }
            },
        )
        .collect();
    out.sort_by(|a, b| {
        b.avg_duration_ms
            .partial_cmp(&a.avg_duration_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(top_k);
    out
}

fn provenance_ops_query_op_label(r: &ProvenanceOpsResource) -> &'static str {
    match r {
        ProvenanceOpsResource::LlmCalls => "ops_query_llm_calls",
        ProvenanceOpsResource::ToolCalls => "ops_query_tool_calls",
        ProvenanceOpsResource::Messages => "ops_query_messages",
        ProvenanceOpsResource::Aggregates => "ops_query_aggregates",
        ProvenanceOpsResource::LifecycleEvents => "ops_query_lifecycle_events",
    }
}

// ---------------------------------------------------------------------------
// Typed per-resource query builders.
//
// Each `build_*_query` chooses a `GraphQuery<labels::Subject, _>` shape
// matching the resource semantics, applies the filterable subset of
// `ProvenanceOpsFilters` via the typed surface, and returns the
// emitted `(SQL, bindings)` pair. There is intentionally no shared
// generic builder — the per-Subject `for_agent` / `for_task` variants
// differ structurally, and the metamodel rejects cross-subject misuse
// at compile time.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Agent package resolution (task-scoped validation + registry instances).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpsAgentPackageResolution {
    None,
    TaskValidatedOmit,
    Empty,
    ApplyInstances,
}

impl SurrealProvenanceStore {
    async fn resolve_ops_agent_package_filter(
        &self,
        filters: &ProvenanceOpsFilters,
        index: &super::agent_runtime_index::AgentRuntimeIndex,
    ) -> Result<OpsAgentPackageResolution> {
        let Some(pkg) = filters.agent_package.as_deref() else {
            return Ok(OpsAgentPackageResolution::None);
        };

        let task_resolution = if let Some(ref task_id) = filters.task_id {
            Some(self.get_task_agent_id(task_id).await?)
        } else {
            None
        };

        match task_agent_package_check(
            filters.task_id.as_ref(),
            Some(pkg),
            task_resolution.as_ref(),
            index,
        ) {
            TaskAgentPackageCheck::OmitAgentFilter => {
                Ok(OpsAgentPackageResolution::TaskValidatedOmit)
            }
            TaskAgentPackageCheck::MismatchEmpty => Ok(OpsAgentPackageResolution::Empty),
            TaskAgentPackageCheck::ApplyPackageFilter => {
                match index.instance_node_ids_by_package.get(pkg) {
                    Some(instances) if instances.is_empty() => Ok(OpsAgentPackageResolution::Empty),
                    Some(_) => Ok(OpsAgentPackageResolution::ApplyInstances),
                    None => Ok(OpsAgentPackageResolution::Empty),
                }
            }
        }
    }
}

fn build_messages_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_message_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    } else {
        let q = GraphQuery::<labels::Message, _>::new().all();
        let q = apply_message_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    }
}

fn apply_message_filters<S: ScopeState + crate::metamodel::query::ScopeQueryEmitter>(
    mut q: GraphQuery<labels::Message, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::Message, S> {
    if let Some(ref task_id) = filters.task_id {
        q = q.for_task(TaskNodeId::for_task_id(task_id));
    }
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else if package_resolution == OpsAgentPackageResolution::ApplyInstances
        && let Some(instances) = package_instances
    {
        q = q.for_agent_instances(instances);
    }
    q
}

fn build_llm_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::LlmCall, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_llm_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    } else {
        let q = GraphQuery::<labels::LlmCall, _>::new().all();
        let q = apply_llm_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    }
}

fn apply_llm_filters<S: ScopeState + crate::metamodel::query::ScopeQueryEmitter>(
    mut q: GraphQuery<labels::LlmCall, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::LlmCall, S> {
    if let Some(ref task_id) = filters.task_id {
        q = q.for_task_execution(TaskExecutionNodeId::for_task_id(task_id));
    }
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else if package_resolution == OpsAgentPackageResolution::ApplyInstances
        && let Some(instances) = package_instances
    {
        q = q.for_agent_instances(instances);
    }
    if let Some(ref provider) = filters.provider {
        q = q.filter(keys::Provider, FilterOp::Eq, provider.clone());
    }
    if let Some(ref model) = filters.model {
        q = q.filter(keys::Model, FilterOp::Eq, model.clone());
    }
    q
}

fn build_tool_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::ToolCall, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_tool_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    } else {
        let q = GraphQuery::<labels::ToolCall, _>::new().all();
        let q = apply_tool_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    }
}

fn apply_tool_filters<S: ScopeState + crate::metamodel::query::ScopeQueryEmitter>(
    mut q: GraphQuery<labels::ToolCall, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::ToolCall, S> {
    if let Some(ref task_id) = filters.task_id {
        q = q.for_task_execution(TaskExecutionNodeId::for_task_id(task_id));
    }
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else if package_resolution == OpsAgentPackageResolution::ApplyInstances
        && let Some(instances) = package_instances
    {
        q = q.for_agent_instances(instances);
    }
    if let Some(ref tool_name) = filters.tool_name {
        q = q.filter(keys::ToolName, FilterOp::Eq, tool_name.clone());
    }
    q
}

fn build_lifecycle_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::AgentStop, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_lifecycle_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    } else {
        let q = GraphQuery::<labels::AgentStop, _>::new().all();
        let q = apply_lifecycle_filters(q, filters, package_resolution, package_instances);
        match sql_page {
            Some((offset, limit, sort_desc)) => {
                apply_sql_page(q, offset, limit, sort_desc).into_surreal()
            }
            None => q.into_surreal(),
        }
    }
}

fn apply_lifecycle_filters<S: ScopeState + crate::metamodel::query::ScopeQueryEmitter>(
    mut q: GraphQuery<labels::AgentStop, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::AgentStop, S> {
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else if package_resolution == OpsAgentPackageResolution::ApplyInstances
        && let Some(instances) = package_instances
    {
        q = q.for_agent_instances(instances);
    }
    q
}

fn apply_sql_page<Subject, S>(
    q: GraphQuery<Subject, S>,
    offset: u64,
    limit: u64,
    sort_desc: bool,
) -> GraphQuery<Subject, S>
where
    Subject: labels::NodeLabelTy,
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter,
{
    use crate::metamodel::query::{SortDir, SortKey};
    q.order_by(
        SortKey::EventOrder,
        if sort_desc {
            SortDir::Desc
        } else {
            SortDir::Asc
        },
    )
    .paginate(offset, limit)
}

fn empty_ops_query_response(
    request: &ProvenanceOpsQueryRequest,
    page_size: usize,
    page_cap: u32,
    requested_page: u32,
) -> ProvenanceOpsQueryResponse {
    use crate::ops_types::ProvenanceOpsSummary;

    let mut summary = ProvenanceOpsSummary::empty();
    if matches!(
        request.resource,
        ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates
    ) {
        summary.cached_input_tokens_total = Some(0);
    }
    ProvenanceOpsQueryResponse {
        resource: request.resource.clone(),
        rows: Vec::new(),
        summary,
        hotspot_groups: Vec::new(),
        next_cursor: None,
        truncated: requested_page > page_cap,
        applied_caps: ProvenanceOpsAppliedCaps {
            page_size: page_size as u32,
            max_page_size: page_cap,
            top_k: request.top_k.unwrap_or(10),
        },
    }
}

#[async_trait]
impl ProvenanceOpsQuery for SurrealProvenanceStore {
    async fn query_ops(
        &self,
        mut request: ProvenanceOpsQueryRequest,
    ) -> Result<ProvenanceOpsQueryResponse> {
        let start = std::time::Instant::now();
        let resource_op = provenance_ops_query_op_label(&request.resource);
        let result = async {
        let profile = if request.budget_mode {
            ProvenanceResponseProfile::ToolCompact
        } else {
            request
                .response_profile
                .clone()
                .unwrap_or(ProvenanceResponseProfile::UiFull)
        };
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
        let sort_desc = parse_ops_sort_dir(request.sort_dir.as_deref())?;
        let sql_paginated = request.group_by.is_empty() || request.paginate_rows_in_sql;
        let sql_page = if sql_paginated {
            Some((
                offset as u64,
                page_size.saturating_add(1) as u64,
                sort_desc,
            ))
        } else if request.budget_mode {
            Some((0, page_size.saturating_mul(25).clamp(500, 2000) as u64, sort_desc))
        } else {
            None
        };

        let agent_runtime_index = self.load_agent_runtime_index().await?;
        let identity_by_agent_id = agent_runtime_index.identity_by_agent_id.clone();
        let package_resolution = self
            .resolve_ops_agent_package_filter(&request.filters, &agent_runtime_index)
            .await?;
        if package_resolution == OpsAgentPackageResolution::Empty {
            return Ok(empty_ops_query_response(
                &request,
                page_size,
                page_cap,
                requested_page,
            ));
        }
        let resolved_package_instances = request.filters.agent_package.as_deref().and_then(|pkg| {
            agent_runtime_index
                .instance_node_ids_by_package
                .get(pkg)
                .cloned()
        });
        let package_instances = match package_resolution {
            OpsAgentPackageResolution::ApplyInstances => resolved_package_instances.as_deref(),
            OpsAgentPackageResolution::None
            | OpsAgentPackageResolution::TaskValidatedOmit
            | OpsAgentPackageResolution::Empty => None,
        };

        // Per-resource typed dispatch. Each `build_*_query` returns a
        // `(SQL, bindings)` pair from `GraphQuery::into_surreal()` — the
        // only legal SQL emitter for graph-targeted reads in this crate.
        // No `format!` of edge labels appears below.
        let (query, binds_value) = match request.resource {
            ProvenanceOpsResource::Messages => build_messages_query(
                &request.filters,
                package_resolution,
                package_instances,
                sql_page,
            ),
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => {
                build_llm_query(
                    &request.filters,
                    package_resolution,
                    package_instances,
                    sql_page,
                )
            }
            ProvenanceOpsResource::ToolCalls => build_tool_query(
                &request.filters,
                package_resolution,
                package_instances,
                sql_page,
            ),
            ProvenanceOpsResource::LifecycleEvents => build_lifecycle_query(
                &request.filters,
                package_resolution,
                package_instances,
                sql_page,
            ),
        };
        let rows = self.execute_typed_node_query(&query, &binds_value).await?;

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
                    let (tool_args, parsed_for_phase) =
                        if let Some(payload_id) =
                            row.get("a2a_tool_call_payload_id").and_then(Value::as_str)
                        {
                            if let Ok(Some(payload)) = self.read_payload_by_id(payload_id).await {
                                let parsed = parse_json_like_string(&payload.payload_json);
                                let args = parsed.get("args").cloned().unwrap_or(Value::Null);
                                (args, Some(parsed))
                            } else {
                                (
                                    parse_json_field(row, "a2a_args").unwrap_or(Value::Null),
                                    None,
                                )
                            }
                        } else {
                            (
                                parse_json_field(row, "a2a_args").unwrap_or(Value::Null),
                                None,
                            )
                        };
                    let phase =
                        resolve_tool_session_phase_for_ops_row(parsed_for_phase.as_ref(), row);
                    row.insert(
                        "tool_call".to_string(),
                        OpsToolCallEnrichment {
                            name: tool_name,
                            args: tool_args,
                            phase,
                        }
                        .into_json_value(),
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
        // `tool_call.phase` is always a string (see [`OpsToolCallEnrichment`]).
        if matches!(request.resource, ProvenanceOpsResource::ToolCalls) {
            ops_rows.retain(|row| {
                let Some(phase_str) = row
                    .get("tool_call")
                    .and_then(|v| v.get("phase"))
                    .and_then(Value::as_str)
                else {
                    return true;
                };
                let phase = ToolSessionPhase::from_metadata(&serde_json::json!({ "phase": phase_str }));
                !matches!(phase, ToolSessionPhase::Open)
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
                    ops_row_is_success(row)
                        || ops_row_is_failed(row)
                        || (matches!(request.resource, ProvenanceOpsResource::ToolCalls)
                            && ops_row_is_indeterminate(row))
                }
            }
        });

        let effective_group_by = parse_ops_group_by(&request.group_by)?;
        let mut sql_budget_truncated = false;
        if sql_page.is_some() && !sql_paginated {
            let cap = sql_page.map(|(_, limit, _)| limit as usize).unwrap_or(0);
            if cap > 0 && ops_rows.len() > cap {
                sql_budget_truncated = true;
                ops_rows.truncate(cap);
            }
        }

        let (page_rows, next_cursor, summary_rows) = if sql_paginated {
            let has_more = ops_rows.len() > page_size;
            if has_more {
                ops_rows.truncate(page_size);
            }
            let next = if has_more {
                Some((offset + page_size).to_string())
            } else {
                None
            };
            let rows = ops_rows.clone();
            (rows, next, ops_rows)
        } else {
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
            (page_rows, next_cursor, ops_rows)
        };

        let mut durations: Vec<f64> = summary_rows
            .iter()
            .filter_map(|r| r.get("duration_ms").and_then(Value::as_f64))
            .collect();
        durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut tokens: Vec<f64> = summary_rows
            .iter()
            .filter_map(|r| r.get("total_tokens").and_then(Value::as_f64))
            .collect();
        tokens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let duration_p95 = percentile(&durations, 0.95);
        let duration_p99 = percentile(&durations, 0.99);
        let token_p95 = percentile(&tokens, 0.95);
        let token_p99 = percentile(&tokens, 0.99);

        let total_rows = summary_rows.len();
        let top_k = request.top_k.unwrap_or(10) as usize;
        let hotspot_groups = build_hotspot_groups(&summary_rows, &effective_group_by, top_k);
        let has_more_page = next_cursor.is_some();
        let failed_count = summary_rows.iter().filter(|r| ops_row_is_failed(r)).count() as u64;
        let include_cached = matches!(
            request.resource,
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates
        );
        let mut summary = build_ops_summary(
            &summary_rows,
            include_cached,
            duration_p95,
            duration_p99,
            token_p95,
            token_p99,
        );
        summary.count = total_rows as u64;
        summary.failed_count = failed_count;

        Ok(ProvenanceOpsQueryResponse {
            resource: request.resource,
            rows: page_rows
                .into_iter()
                .map(ProvenanceOpsRow::from_map)
                .collect(),
            summary,
            hotspot_groups,
            next_cursor,
            truncated: sql_budget_truncated
                || has_more_page
                || total_rows > page_size
                || requested_page > page_cap,
            applied_caps: ProvenanceOpsAppliedCaps {
                page_size: page_size as u32,
                max_page_size: page_cap,
                top_k: request.top_k.unwrap_or(10),
            },
        })
        }.await;
        let result_label = match &result {
            Ok(_) => "success",
            Err(_) => "error",
        };
        baml_rt_observability::record_provenance_read(resource_op, result_label, start.elapsed());
        result
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
