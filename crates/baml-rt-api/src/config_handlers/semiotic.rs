// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Semiotic gate config bundle (`GET/PUT /config/semiotic`) and operator sub-resources.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
};
use baml_rt_config::ConfigReader;
use baml_rt_provenance::{
    AgentGateActivity, AgentGateCounts, GATE_ACTIVITY_MAX_ROWS, GateIncidentRow, RankedCount,
    agent_has_gate_activity,
};
use baml_rt_semiotic::{
    EffectiveAgentPolicy, EffectiveSystemPolicy, SEMIOTIC_CONFIG_BUNDLE_NAME, SemioticConfig,
    SemioticPosture, semiotic_bundle_schema, set_global_semiotic_config,
};
use baml_rt_tools::BundleName;
use http_api_problem::HttpApiProblem;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{HttpResult, config_err_500, problem};
use crate::{openapi::ToolConfigSchemaDto, router::ApiState};

pub fn is_bundle(name: &str) -> bool {
    name == SEMIOTIC_CONFIG_BUNDLE_NAME
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticEffectiveDto {
    pub version: u64,
    pub system: EffectiveSystemPolicy,
    pub agents: Vec<EffectiveAgentPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticEffectivePolicyRef {
    pub posture: SemioticPosture,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateIncidentDrillDto {
    pub context_id: String,
    pub task_id: String,
    pub tool_call_anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticIncidentDto {
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
    pub drill: GateIncidentDrillDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticAgentActivityDto {
    pub agent_package: String,
    pub effective: SemioticEffectivePolicyRef,
    pub counts: AgentGateCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prevention_ratio: Option<f32>,
    pub top_reason_codes: Vec<RankedCount>,
    pub top_deficient_nodes: Vec<RankedCount>,
    pub recent_incidents: Vec<SemioticIncidentDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticFleetActivityDto {
    pub deny_count: u32,
    pub ask_count: u32,
    pub friction_denial_count: u32,
    pub prevented_error_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prevention_ratio: Option<f32>,
    pub agents_with_activity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticActivityDto {
    pub window_hours: u32,
    pub since_ms: u64,
    pub until_ms: u64,
    pub config_version: u64,
    pub fleet: SemioticFleetActivityDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_reason: Option<String>,
    pub agents: Vec<SemioticAgentActivityDto>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemioticActivityQuery {
    #[serde(default = "default_window_hours")]
    pub window_hours: u32,
    pub agent_package: Option<String>,
    #[serde(default = "default_incident_limit")]
    pub limit: u32,
}

fn default_window_hours() -> u32 {
    24
}

fn default_incident_limit() -> u32 {
    20
}

#[expect(
    clippy::result_large_err,
    reason = "HttpApiProblem is the HttpResult error type; boxing it would ripple through every handler signature"
)]
pub fn list_schema_entry(has_config: bool) -> Result<ToolConfigSchemaDto, HttpApiProblem> {
    let default_semiotic_value = serde_json::to_value(SemioticConfig::default()).map_err(|e| {
        problem(
            500,
            "Internal Error",
            format!("serialize default semiotic config: {e}"),
        )
    })?;
    Ok(ToolConfigSchemaDto {
        tool_name: SEMIOTIC_CONFIG_BUNDLE_NAME.to_string(),
        schema: semiotic_bundle_schema(),
        default: Some(default_semiotic_value),
        has_config,
    })
}

pub async fn load_or_default(
    config: &dyn ConfigReader,
    parsed: &BundleName,
) -> Result<(Value, u64), HttpApiProblem> {
    match config
        .get_with_version(parsed)
        .await
        .map_err(config_err_500)?
    {
        Some(s) => Ok((s.config, s.version.into())),
        None => {
            let value = serde_json::to_value(SemioticConfig::default()).map_err(|e| {
                problem(
                    500,
                    "Internal Error",
                    format!("serialize default semiotic config: {e}"),
                )
            })?;
            Ok((value, 0))
        }
    }
}

async fn load_semiotic_config(state: &ApiState) -> Result<(SemioticConfig, u64), HttpApiProblem> {
    let parsed = BundleName::new(SEMIOTIC_CONFIG_BUNDLE_NAME)
        .map_err(|e| problem(500, "Internal Error", format!("parse semiotic bundle: {e}")))?;
    let (value, version) = load_or_default(state.config_service.as_ref(), &parsed).await?;
    let config = SemioticConfig::from_value(value).map_err(|e| {
        problem(
            400,
            "Invalid config",
            format!("stored semiotic config: {e}"),
        )
    })?;
    Ok((config, version))
}

fn discovered_packages(state: &ApiState) -> Vec<String> {
    let mut packages: Vec<String> = state
        .registry
        .list_agents()
        .into_iter()
        .map(|e| e.agent_package)
        .collect();
    packages.sort();
    packages.dedup();
    packages
}

/// Resolved semiotic policies for system default and each agent package.
#[utoipa::path(
    get,
    path = "/config/semiotic/effective",
    tag = "config",
    security(("RunnerToken" = [])),
    responses(
        (status = 200, description = "Resolved semiotic gate policies"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn get_semiotic_effective(
    State(state): State<Arc<ApiState>>,
) -> HttpResult<SemioticEffectiveDto> {
    let start = std::time::Instant::now();
    let result = async {
        let (config, version) = load_semiotic_config(&state).await?;
        let packages = discovered_packages(&state);
        Ok(Json(SemioticEffectiveDto {
            version,
            system: config.effective_system(),
            agents: config.effective_agents(&packages),
        }))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_semiotic_effective", start, &result);
    result
}

fn incident_to_dto(row: GateIncidentRow) -> SemioticIncidentDto {
    SemioticIncidentDto {
        occurred_at_ms: row.occurred_at_ms,
        context_id: row.context_id.clone(),
        task_id: row.task_id.clone(),
        tool_name: row.tool_name,
        tier: row.tier,
        decision: row.decision,
        reason_code: row.reason_code,
        deficient_nodes: row.deficient_nodes,
        telemetry_verdict: row.telemetry_verdict,
        severity: row.severity,
        drill: GateIncidentDrillDto {
            context_id: row.context_id,
            task_id: row.task_id,
            tool_call_anchor: row.tool_call_anchor,
        },
    }
}

fn agent_activity_to_dto(
    activity: AgentGateActivity,
    effective: &EffectiveAgentPolicy,
) -> SemioticAgentActivityDto {
    SemioticAgentActivityDto {
        agent_package: activity.agent_package,
        effective: SemioticEffectivePolicyRef {
            posture: effective.posture,
            summary: effective.summary.clone(),
        },
        counts: activity.counts,
        prevention_ratio: activity.prevention_ratio,
        top_reason_codes: activity.top_reason_codes,
        top_deficient_nodes: activity.top_deficient_nodes,
        recent_incidents: activity
            .recent_incidents
            .into_iter()
            .map(incident_to_dto)
            .collect(),
    }
}

fn activity_package_list(
    state: &ApiState,
    query: &SemioticActivityQuery,
    activity_map: &std::collections::HashMap<String, AgentGateActivity>,
) -> Vec<String> {
    if let Some(ref pkg) = query.agent_package {
        return vec![pkg.clone()];
    }
    let mut packages: std::collections::BTreeSet<String> =
        discovered_packages(state).into_iter().collect();
    for pkg in activity_map.keys() {
        packages.insert(pkg.clone());
    }
    packages.into_iter().collect()
}

fn fleet_from_activity_map(
    activity_map: &std::collections::HashMap<String, AgentGateActivity>,
) -> SemioticFleetActivityDto {
    let mut fleet = SemioticFleetActivityDto {
        deny_count: 0,
        ask_count: 0,
        friction_denial_count: 0,
        prevented_error_count: 0,
        prevention_ratio: None,
        agents_with_activity: 0,
    };
    for activity in activity_map.values() {
        if agent_has_gate_activity(activity) {
            fleet.agents_with_activity += 1;
        }
        fleet.deny_count += activity.counts.deny;
        fleet.ask_count += activity.counts.ask;
        fleet.friction_denial_count += activity.counts.friction_denial;
        fleet.prevented_error_count += activity.counts.prevented_error;
    }
    let denom = fleet.prevented_error_count + fleet.friction_denial_count;
    if denom > 0 {
        fleet.prevention_ratio = Some(fleet.prevented_error_count as f32 / denom as f32);
    }
    fleet
}

/// Provenance-backed gate activity for operator incident diagnosis.
#[utoipa::path(
    get,
    path = "/config/semiotic/activity",
    tag = "config",
    security(("RunnerToken" = [])),
    params(
        ("windowHours" = Option<u32>, Query, description = "Rolling lookback hours (max 168)"),
        ("agentPackage" = Option<String>, Query, description = "Filter to one agent package"),
        ("limit" = Option<u32>, Query, description = "Max recent incidents per agent (max 50)")
    ),
    responses(
        (status = 200, description = "Gate activity rollup"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 503, description = "Provenance or config unavailable")
    )
)]
pub async fn get_semiotic_activity(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SemioticActivityQuery>,
) -> HttpResult<SemioticActivityDto> {
    let start = std::time::Instant::now();
    let result = async {
        let provenance = state.provenance_ops.as_ref().ok_or_else(|| {
            problem(
                503,
                "Service Unavailable",
                "provenance ops service unavailable",
            )
        })?;

        let (config, version) = load_semiotic_config(&state).await?;
        let window_hours = query.window_hours.clamp(1, 168);
        let incident_limit = query.limit.clamp(1, 50) as usize;
        let until_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let since_ms = until_ms.saturating_sub(window_hours as u64 * 3_600_000);

        let (activity_map, truncated) = provenance
            .aggregate_gate_activity(baml_rt_provenance::AgentGateActivityFilters {
                agent_package: query.agent_package.clone(),
                from_timestamp_ms: since_ms,
                to_timestamp_ms: until_ms,
                incident_limit,
                page_size: GATE_ACTIVITY_MAX_ROWS as u32,
            })
            .await
            .map_err(|e| {
                problem(
                    503,
                    "Service Unavailable",
                    format!("provenance gate activity query failed: {e}"),
                )
            })?;

        let packages = activity_package_list(&state, &query, &activity_map);
        let effective_agents = config.effective_agents(&packages);
        let fleet = fleet_from_activity_map(&activity_map);
        let single_agent = query.agent_package.is_some();

        let mut agents = Vec::new();
        for eff in &effective_agents {
            let activity =
                activity_map
                    .get(&eff.agent_package)
                    .cloned()
                    .unwrap_or(AgentGateActivity {
                        agent_package: eff.agent_package.clone(),
                        counts: AgentGateCounts::default(),
                        prevention_ratio: None,
                        top_reason_codes: Vec::new(),
                        top_deficient_nodes: Vec::new(),
                        recent_incidents: Vec::new(),
                    });
            if single_agent || agent_has_gate_activity(&activity) {
                agents.push(agent_activity_to_dto(activity, eff));
            }
        }

        let empty_reason = if !config.default.enabled {
            Some(
                "Semiotic gate is disabled — enable in Trust settings to record evaluations."
                    .to_string(),
            )
        } else if config.default.posture() == SemioticPosture::Audit
            && fleet.agents_with_activity == 0
        {
            Some("Dry-run mode — gate records decisions without blocking tool calls.".to_string())
        } else if fleet.agents_with_activity == 0 {
            Some("No gate evaluations in the selected time window.".to_string())
        } else {
            None
        };

        Ok(Json(SemioticActivityDto {
            window_hours,
            since_ms,
            until_ms,
            config_version: version,
            fleet,
            empty_reason,
            agents,
            truncated,
        }))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_semiotic_activity", start, &result);
    result
}

#[expect(
    clippy::result_large_err,
    reason = "HttpApiProblem is the HttpResult error type; boxing it would ripple through every handler signature"
)]
pub fn parse_put_body(body: Value) -> Result<Value, HttpApiProblem> {
    let semiotic_config: SemioticConfig = serde_json::from_value(body)
        .map_err(|e| problem(400, "Invalid config", format!("Semiotic config: {e}")))?;
    serde_json::to_value(&semiotic_config).map_err(|e| {
        problem(
            500,
            "Internal Error",
            format!("serialize semiotic config: {e}"),
        )
    })
}

pub fn apply_hot_reload(config_value: &Value) {
    if let Ok(cfg) = SemioticConfig::from_value(config_value.clone()) {
        set_global_semiotic_config(cfg);
    }
}

pub fn reset_hot_reload() {
    set_global_semiotic_config(SemioticConfig::default());
}
