// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Unified operator observation bundle and streaming contract.

use std::{error::Error, fmt};

use async_trait::async_trait;
use baml_rt_core::{
    ObservationUpdate,
    ids::{ContextId, ExternalId, TaskId},
};
use baml_rt_provenance::store::ProvenanceOpsQueryResponse;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub type ObservationError = crate::service_error::ServiceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObservationInclude {
    pub planning: bool,
    pub llm_ops: bool,
    pub tool_ops: bool,
    pub drift: bool,
    pub gate: bool,
}

impl ObservationInclude {
    pub fn from_query(
        raw: Option<&str>,
        include_drift: bool,
        include_gate: bool,
    ) -> Result<Self, ObservationRequestParseError> {
        let gate = include_gate || include_drift;
        let mut out = Self {
            planning: true,
            llm_ops: true,
            tool_ops: true,
            drift: include_drift,
            gate,
        };
        let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(out);
        };
        out.planning = false;
        out.llm_ops = false;
        out.tool_ops = false;
        out.drift = false;
        for part in raw.split(',') {
            match part.trim().to_ascii_lowercase().as_str() {
                "planning" => out.planning = true,
                "llmops" | "llm_ops" | "llm" => out.llm_ops = true,
                "toolops" | "tool_ops" | "tool" => out.tool_ops = true,
                "drift" => out.drift = true,
                "gate" => out.gate = true,
                "all" => {
                    out.planning = true;
                    out.llm_ops = true;
                    out.tool_ops = true;
                    out.drift = include_drift;
                    out.gate = gate;
                }
                other if !other.is_empty() => {
                    return Err(ObservationRequestParseError::UnknownInclude(
                        other.to_string(),
                    ));
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ObservationQueryParams {
    pub task_id: Option<String>,
    pub agent_package: Option<String>,
    pub agent_id: Option<String>,
    /// Comma-separated: planning, llmOps, toolOps, drift, all
    pub include: Option<String>,
    pub include_drift: Option<bool>,
    pub include_gate: Option<bool>,
    pub ops_page_size: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ObservationRequest {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub agent_package: Option<String>,
    pub agent_id: Option<baml_rt_core::ids::AgentId>,
    pub include: ObservationInclude,
    pub ops_page_size: u32,
    pub planning_history_limit: u32,
}

impl ObservationRequest {
    pub fn from_parts(
        context_id: &str,
        params: ObservationQueryParams,
    ) -> Result<Self, ObservationRequestParseError> {
        let context_id = ContextId::from(context_id);
        let task_id = params
            .task_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| TaskId::from_external(ExternalId::new(s.to_string())));
        let agent_id = params
            .agent_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(parse_agent_id)
            .transpose()?;
        let include_drift = params.include_drift.unwrap_or(false);
        let include_gate = params.include_gate.unwrap_or(include_drift);
        Ok(Self {
            context_id,
            task_id,
            agent_package: params.agent_package.filter(|s| !s.is_empty()),
            agent_id,
            include: ObservationInclude::from_query(
                params.include.as_deref(),
                include_drift,
                include_gate,
            )?,
            ops_page_size: params.ops_page_size.unwrap_or(20).clamp(1, 50),
            planning_history_limit: crate::planning::DEFAULT_PLANNING_HISTORY_LIMIT,
        })
    }
}

fn parse_agent_id(raw: &str) -> Result<baml_rt_core::ids::AgentId, ObservationRequestParseError> {
    baml_rt_core::ids::UuidId::parse_str(raw)
        .map(baml_rt_core::ids::AgentId::from_uuid)
        .map_err(|e| ObservationRequestParseError::InvalidAgentId(e.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationRequestParseError {
    InvalidAgentId(String),
    UnknownInclude(String),
}

impl fmt::Display for ObservationRequestParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAgentId(e) => write!(f, "invalid agentId: {e}"),
            Self::UnknownInclude(token) => write!(f, "unknown observe include token: {token}"),
        }
    }
}

impl Error for ObservationRequestParseError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationBundleDto {
    pub context_id: String,
    pub version: String,
    pub planning: Option<crate::ContextPlanningResponse>,
    pub llm_ops: Option<ProvenanceOpsQueryResponse>,
    pub tool_ops: Option<ProvenanceOpsQueryResponse>,
}

#[async_trait]
pub trait ObservationService: Send + Sync {
    async fn bundle(
        &self,
        request: ObservationRequest,
    ) -> Result<ObservationBundleDto, ObservationError>;
}

#[async_trait]
pub trait ObservationEventService: Send + Sync {
    fn subscribe_updates(&self) -> tokio::sync::broadcast::Receiver<ObservationUpdate>;
}

pub fn update_matches_request(update: &ObservationUpdate, request: &ObservationRequest) -> bool {
    if update.context_id != request.context_id.as_str() {
        return false;
    }
    if let Some(ref req_task) = request.task_id {
        return update.task_id.as_deref() == Some(req_task.as_str());
    }
    true
}

pub fn update_affects_include(update: &ObservationUpdate, include: ObservationInclude) -> bool {
    if update.affects_planning() && (include.planning || include.drift) {
        return true;
    }
    if update.affects_ops() && (include.llm_ops || include.tool_ops) {
        return true;
    }
    false
}
