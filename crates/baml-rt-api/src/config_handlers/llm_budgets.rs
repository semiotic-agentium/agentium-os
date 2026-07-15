// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! LLM model budget handlers.

use std::sync::Arc;

use axum::{Json, extract::State};
use baml_rt_config::ConfigReader;
use baml_rt_llm_config::{
    LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, ResolvedClientBudgets, clear_online_budget_cache,
    refresh_online_budget_cache, resolve_all_client_budgets,
};
use baml_rt_tools::BundleName;
use http_api_problem::HttpApiProblem;

use super::common::{HttpResult, config_err_500, problem};

async fn load_llm_client_config(
    config: &dyn ConfigReader,
) -> Result<LlmClientConfig, HttpApiProblem> {
    let parsed = BundleName::new(LLM_CONFIG_BUNDLE_NAME).map_err(|e| {
        problem(
            500,
            "Internal Error",
            format!("invalid llm bundle name: {e}"),
        )
    })?;
    let value = match config.get(&parsed).await.map_err(config_err_500)? {
        Some(v) => v,
        None => serde_json::to_value(LlmClientConfig::sensible_default()).map_err(|e| {
            problem(
                500,
                "Internal Error",
                format!("serialize default LLM config: {e}"),
            )
        })?,
    };
    LlmClientConfig::from_value(value).map_err(|e| {
        problem(
            400,
            "Bad Request",
            format!("invalid stored LLM config: {e}"),
        )
    })
}

/// Resolved model compaction budgets for configured LLM clients.
#[utoipa::path(
    get,
    path = "/config/llm/model-budgets",
    tag = "config",
    security(("RunnerToken" = [])),
    responses(
        (status = 200, description = "Resolved compaction budgets"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn get_llm_model_budgets(
    State(state): State<Arc<crate::router::ApiState>>,
) -> HttpResult<ResolvedClientBudgets> {
    let start = std::time::Instant::now();
    let result = async {
        let llm_config = load_llm_client_config(state.config_service.as_ref()).await?;
        Ok(Json(resolve_all_client_budgets(&llm_config)))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_llm_model_budgets", start, &result);
    result
}

#[derive(Debug, serde::Serialize)]
pub struct RefreshModelBudgetsResponse {
    pub updated: usize,
    pub budgets: ResolvedClientBudgets,
}

/// Refresh online model metadata and return resolved compaction budgets.
#[utoipa::path(
    post,
    path = "/config/llm/model-budgets/refresh",
    tag = "config",
    security(("RunnerToken" = [])),
    responses(
        (status = 200, description = "Refreshed compaction budgets"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn refresh_llm_model_budgets(
    State(state): State<Arc<crate::router::ApiState>>,
) -> HttpResult<RefreshModelBudgetsResponse> {
    let start = std::time::Instant::now();
    let result = async {
        let llm_config = load_llm_client_config(state.config_service.as_ref()).await?;
        clear_online_budget_cache();
        let updated = refresh_online_budget_cache(&llm_config).await;
        let mut budgets = resolve_all_client_budgets(&llm_config);
        budgets.refreshed_at_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        );
        Ok(Json(RefreshModelBudgetsResponse { updated, budgets }))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_llm_model_budgets_refresh", start, &result);
    result
}
