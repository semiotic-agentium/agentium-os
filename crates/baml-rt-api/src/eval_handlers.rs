// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Eval session API and dev-artifacts for external SDK.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
};
pub use baml_rt_repository::DevArtifactsBundle;
use baml_rt_repository::{RepositoryService, resolve_package_hash};
use http_api_problem::HttpApiProblem;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::router::ApiState;

#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EvalSessionSpec {
    pub agent: String,
    pub model: Option<String>,
    pub client: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EvalSessionCreated {
    pub eval_session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevArtifactsQuery {
    pub agent: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DevArtifactsResponse {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baml_runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baml_runtime_dts: Option<String>,
}

/// POST /eval/sessions — ephemeral model override scope for eval runs.
#[utoipa::path(
    post,
    path = "/eval/sessions",
    tag = "eval",
    security(("RunnerToken" = [])),
    request_body = EvalSessionSpec,
    responses(
        (status = 200, description = "Eval session created", body = EvalSessionCreated),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn post_eval_session(
    State(state): State<Arc<ApiState>>,
    Json(spec): Json<EvalSessionSpec>,
) -> Result<Json<EvalSessionCreated>, HttpApiProblem> {
    let id = format!("eval-{}", Uuid::new_v4());
    state.eval_sessions.insert(id.clone(), spec).map_err(|_| {
        HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
            .title("Eval session store poisoned")
    })?;
    Ok(Json(EvalSessionCreated {
        eval_session_id: id,
    }))
}

/// GET /repository/dev-artifacts — server-generated prelude + TypeScript stubs from publish build.
#[utoipa::path(
    get,
    path = "/repository/dev-artifacts",
    tag = "repository",
    params(
        ("agent" = Option<String>, Query, description = "Agent package name"),
        ("hash" = Option<String>, Query, description = "Content hash from publish")
    ),
    responses((status = 200, description = "Dev artifact bundle", body = DevArtifactsResponse))
)]
pub async fn get_dev_artifacts(
    State(svc): State<Arc<RepositoryService>>,
    Query(query): Query<DevArtifactsQuery>,
) -> Json<DevArtifactsResponse> {
    let package_hash =
        match resolve_package_hash(svc.as_ref(), query.agent.as_deref(), query.hash.as_deref())
            .await
        {
            Ok(Some(hash)) => hash,
            Ok(None) => {
                return Json(DevArtifactsResponse {
                    status: "not_found",
                    baml_runtime: None,
                    baml_runtime_dts: None,
                });
            }
            Err(_) => {
                return Json(DevArtifactsResponse {
                    status: "not_found",
                    baml_runtime: None,
                    baml_runtime_dts: None,
                });
            }
        };

    match svc.get_dev_artifacts(&package_hash).await {
        Ok(Some(bundle)) => Json(DevArtifactsResponse {
            status: "ok",
            baml_runtime: Some(bundle.baml_runtime),
            baml_runtime_dts: Some(bundle.baml_runtime_dts),
        }),
        Ok(None) => Json(DevArtifactsResponse {
            status: "not_implemented",
            baml_runtime: None,
            baml_runtime_dts: None,
        }),
        Err(_) => Json(DevArtifactsResponse {
            status: "not_implemented",
            baml_runtime: None,
            baml_runtime_dts: None,
        }),
    }
}

/// Resolve eval session model override if header present.
pub fn lookup_eval_session(state: &ApiState, session_id: &str) -> Option<EvalSessionSpec> {
    state.eval_sessions.get(session_id)
}
