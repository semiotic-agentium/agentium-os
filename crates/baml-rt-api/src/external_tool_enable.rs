// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Runner-owned external-tool approval/discovery endpoint.

use std::{path::PathBuf, sync::Arc};

use axum::extract::{Json, State};
use baml_rt_repository::{ExternalToolRegistryToolVersion, RepositoryService};
use baml_rt_tools::{
    approval::ApprovalState,
    external_tools::{
        ExternalToolSnapshot, discover_snapshot, now_snapshot_timestamp,
        resolver::SandboxRuntimeWiring,
    },
};
use http_api_problem::HttpApiProblem;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct ExternalToolEnableState {
    pub repository: Arc<RepositoryService>,
    pub sandbox: Option<SandboxRuntimeWiring>,
}

/// Request body for runner-owned external tool enable.
#[derive(Debug, Deserialize, Serialize)]
pub struct EnableExternalToolRequest {
    pub tool_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_rootfs: Option<PathBuf>,
    /// Audit identity recorded as the approval owner (the "who"). Self-asserted:
    /// the operator token is shared and carries no per-user identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EnableExternalToolResponse {
    pub snapshot: ExternalToolSnapshot,
    pub version: ExternalToolRegistryToolVersion,
}

/// Discover an external tool inside runner runtime, approve snapshot, store registry version.
pub async fn enable_external_tool(
    State(state): State<ExternalToolEnableState>,
    Json(req): Json<EnableExternalToolRequest>,
) -> Result<Json<EnableExternalToolResponse>, HttpApiProblem> {
    let snapshot = discover_snapshot(&req.tool_dir, req.sandbox_rootfs, state.sandbox.as_ref())
        .await
        .map_err(|e| {
            HttpApiProblem::new(http_api_problem::StatusCode::BAD_REQUEST)
                .title("External tool discovery failed")
                .detail(e.to_string())
        })?;
    // NOTE: single-step approval — the runner discovers, approves, and imports
    // in one call, so the caller blesses the snapshot before seeing the schema
    // the runner actually computed (which can differ from the operator's local
    // view: sandbox vs process env, rootfs contents, tool version drift).
    //
    // Accepted for now: discovery is deterministic, a bad tool surfaces as an
    // error here, and an imported snapshot can be inspected and `mark-stale`d
    // after the fact.
    //
    // TODO: promote to a two-step flow when review-before-commit is needed —
    // store as `ApprovalState::Pending`, return the computed snapshot for the
    // operator to inspect, and add a separate approve call that flips
    // Pending -> Approved. The `ApprovalState::Pending` variant + `mark-stale`
    // machinery already exist to support this.
    let approved = approved(snapshot, req.approved_by);
    let version = state
        .repository
        .put_external_tool_snapshot(&approved)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(EnableExternalToolResponse {
        snapshot: approved,
        version,
    }))
}

fn approved(
    mut snapshot: ExternalToolSnapshot,
    approved_by: Option<String>,
) -> ExternalToolSnapshot {
    snapshot.approval.state = ApprovalState::Approved;
    snapshot.approval.reviewed_at = Some(now_snapshot_timestamp());
    snapshot.approval.owner = approved_by;
    snapshot
}
