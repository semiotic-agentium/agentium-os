// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Config bundle CRUD handlers.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode as AxumStatus};
use baml_rt_llm_config::{LLM_CONFIG_BUNDLE_NAME, LlmClientConfig};
use baml_rt_semiotic::SEMIOTIC_CONFIG_BUNDLE_NAME;
use baml_rt_tools::BundleName;
use http_api_problem::HttpApiProblem;
use serde_json::Value;

use super::{
    common::{HttpResult, config_err_500, is_builtin_config_bundle, llm_bundle_schema, problem},
    semiotic,
};
use crate::openapi::{ConfigVersionDto, ToolConfigDto, ToolConfigSchemaDto};

/// List bundles with config schema and whether each has stored config (GET /config).
#[utoipa::path(
    get,
    path = "/config",
    tag = "config",
    security(("RunnerToken" = [])),
    responses(
        (status = 200, description = "List of bundles with config schema", body = Vec<ToolConfigSchemaDto>),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn list_config(
    State(state): State<Arc<crate::router::ApiState>>,
) -> HttpResult<Vec<ToolConfigSchemaDto>> {
    let start = std::time::Instant::now();
    let result = async {
        let catalog = &state.tool_catalog;
        let config = &state.config_service;

        let with_config: std::collections::HashSet<String> = config
            .list_with_config()
            .await
            .map_err(config_err_500)?
            .into_iter()
            .map(|b| b.as_str().to_string())
            .collect();

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();

        // Always expose the LLM config bundle so the UI can show the LLM section.
        // LLM_CONFIG_BUNDLE_NAME is a crate constant guaranteed by baml_rt_llm_config to pass BundleName::new.
        let llm_bundle = BundleName::new(LLM_CONFIG_BUNDLE_NAME).expect("llm bundle name is valid");
        seen.insert(llm_bundle.clone());
        let default_llm_value =
            serde_json::to_value(LlmClientConfig::sensible_default()).map_err(|e| {
                problem(
                    500,
                    "Internal Error",
                    format!("serialize default LLM config: {e}"),
                )
            })?;
        out.push(ToolConfigSchemaDto {
            tool_name: LLM_CONFIG_BUNDLE_NAME.to_string(),
            schema: llm_bundle_schema(),
            default: Some(default_llm_value),
            has_config: with_config.contains(LLM_CONFIG_BUNDLE_NAME),
        });

        let semiotic_bundle =
            BundleName::new(SEMIOTIC_CONFIG_BUNDLE_NAME).expect("semiotic bundle name is valid");
        seen.insert(semiotic_bundle);
        out.push(semiotic::list_schema_entry(
            with_config.contains(SEMIOTIC_CONFIG_BUNDLE_NAME),
        )?);

        for metadata in catalog.iter() {
            let Some(ref config_bundle) = metadata.config_bundle else {
                continue;
            };
            let Some(ref config_meta) = metadata.config else {
                continue;
            };
            if !seen.insert(config_bundle.clone()) {
                continue;
            }
            let bundle_name = config_bundle.as_str().to_string();
            let has_config = with_config.contains(config_bundle.as_str());
            out.push(ToolConfigSchemaDto {
                tool_name: bundle_name,
                schema: config_meta.schema.clone(),
                default: Some(config_meta.default.clone()),
                has_config,
            });
        }

        Ok(Json(out))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_list", start, &result);
    result
}

/// Get config schema + current config for a bundle (GET /config/{bundle_name}).
#[utoipa::path(
    get,
    path = "/config/{bundle_name}",
    tag = "config",
    security(("RunnerToken" = [])),
    params(("bundle_name" = String, Path, description = "Bundle name (e.g. llm or a tool bundle that has config)")),
    responses(
        (status = 200, description = "Config schema and current config", body = ToolConfigDto),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Bundle not found or has no config schema"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn get_config(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path(bundle_name): axum::extract::Path<String>,
) -> HttpResult<ToolConfigDto> {
    let start = std::time::Instant::now();
    let result = async {
        let catalog = &state.tool_catalog;
        let config = &state.config_service;

        let parsed = BundleName::new(&bundle_name).map_err(|_| {
            problem(
                400,
                "Bad Request",
                format!("Invalid bundle name: {bundle_name}"),
            )
        })?;

        let (config_value, version) = if parsed.as_str() == LLM_CONFIG_BUNDLE_NAME {
            match config
                .get_with_version(&parsed)
                .await
                .map_err(config_err_500)?
            {
                Some(s) => (s.config, s.version.into()),
                None => (
                    serde_json::to_value(LlmClientConfig::sensible_default()).map_err(|e| {
                        problem(
                            500,
                            "Internal Error",
                            format!("serialize default LLM config: {e}"),
                        )
                    })?,
                    0,
                ),
            }
        } else if semiotic::is_bundle(parsed.as_str()) {
            let (config_value, version) =
                semiotic::load_or_default(config.as_ref(), &parsed).await?;
            (config_value, version)
        } else {
            let metadata = catalog.bundle_config(&parsed).ok_or_else(|| {
                problem(
                    404,
                    "Not Found",
                    format!("Bundle '{bundle_name}' not found or has no config schema"),
                )
            })?;

            let config_meta = metadata.config.as_ref().ok_or_else(|| {
                problem(
                    404,
                    "Not Found",
                    format!("Bundle '{bundle_name}' has no config schema"),
                )
            })?;

            match config
                .get_with_version(&parsed)
                .await
                .map_err(config_err_500)?
            {
                Some(s) => (s.config, s.version.into()),
                None => (config_meta.default.clone(), 0),
            }
        };

        Ok(Json(ToolConfigDto {
            tool_name: bundle_name,
            config: config_value,
            version,
        }))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_get", start, &result);
    result
}

/// Create or update config (PUT /config/{bundle_name}).
/// Request body shape is defined by the bundle's config schema (GET /config/{bundle_name} returns it).
/// Send `If-Match: <version>` to enable optimistic concurrency; returns 409 if stale.
#[utoipa::path(
    put,
    path = "/config/{bundle_name}",
    tag = "config",
    security(("RunnerToken" = [])),
    params(("bundle_name" = String, Path, description = "Bundle name (e.g. llm or a tool bundle that has config)")),
    request_body(content = Value, description = "Config JSON (must match bundle schema from GET /config/{bundle_name})", content_type = "application/json"),
    responses(
        (status = 200, description = "Config updated", body = ConfigVersionDto),
        (status = 400, description = "Invalid config"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Bundle not found"),
        (status = 409, description = "Version conflict (stale If-Match)"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn put_config(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path(bundle_name): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> HttpResult<ConfigVersionDto> {
    let start = std::time::Instant::now();
    let result = async {
    let catalog = &state.tool_catalog;
    let config = &state.config_service;

    let parsed = BundleName::new(&bundle_name).map_err(|_| {
        problem(
            400,
            "Bad Request",
            format!("Invalid bundle name: {bundle_name}"),
        )
    })?;

    let body_to_store = if parsed.as_str() == LLM_CONFIG_BUNDLE_NAME {
        let mut llm_config: LlmClientConfig = serde_json::from_value(body)
            .map_err(|e| problem(400, "Invalid config", format!("LLM config: {e}")))?;
        llm_config.normalize();
        serde_json::to_value(&llm_config)
            .map_err(|e| problem(500, "Internal Error", format!("serialize LLM config: {e}")))?
    } else if semiotic::is_bundle(parsed.as_str()) {
        semiotic::parse_put_body(body)?
    } else {
        let config_meta = catalog
            .bundle_config(&parsed)
            .and_then(|m| m.config.as_ref())
            .ok_or_else(|| {
                problem(
                    404,
                    "Not Found",
                    format!("Bundle '{bundle_name}' not found or has no config schema"),
                )
            })?;

        if let Ok(validator) = jsonschema::JSONSchema::compile(&config_meta.schema)
            && let Err(err_iter) = validator.validate(&body)
        {
            let msg: String = err_iter
                .map(|e| format!("{}: {}", e.instance_path, e))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(problem(400, "Invalid config", msg));
        }
        body
    };

    if let Some(if_match) = headers.get("if-match").and_then(|v| v.to_str().ok()) {
        let expected: u64 = if_match.trim().parse().map_err(|_| {
            problem(
                400,
                "Bad Request",
                format!("If-Match must be a version number, got: {if_match}"),
            )
        })?;
        let current = config
            .get_with_version(&parsed)
            .await
            .map_err(config_err_500)?
            .map(|s| s.version.into())
            .unwrap_or(0u64);
        if current != expected {
            return Err(problem(
                409,
                "Conflict",
                format!(
                    "Version conflict: you have version {expected}, but current is {current}. Reload and retry."
                ),
            ));
        }
    }

    let version = config.set(&parsed, body_to_store).await.map_err(|e| {
        tracing::error!(error = %e, "config set failed");
        problem(400, "Bad Request", e.to_string())
    })?;

    if semiotic::is_bundle(parsed.as_str()) {
        semiotic::apply_hot_reload(&version.config);
    }

    Ok(Json(ConfigVersionDto {
        version: version.version.into(),
        config: version.config,
        created_at_ms: version.created_at_ms.into(),
    }))
    }.await;
    crate::metrics::finish_json_http_metrics("config_put", start, &result);
    result
}

/// Remove stored config (DELETE /config/{bundle_name}).
#[utoipa::path(
    delete,
    path = "/config/{bundle_name}",
    tag = "config",
    security(("RunnerToken" = [])),
    params(("bundle_name" = String, Path, description = "Bundle name (e.g. llm or a tool bundle that has config)")),
    responses(
        (status = 204, description = "Config removed"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Bundle not found"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn delete_config(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path(bundle_name): axum::extract::Path<String>,
) -> Result<AxumStatus, HttpApiProblem> {
    let start = std::time::Instant::now();
    let result = async {
        let catalog = &state.tool_catalog;
        let config = &state.config_service;

        let parsed = BundleName::new(&bundle_name).map_err(|_| {
            problem(
                400,
                "Bad Request",
                format!("Invalid bundle name: {bundle_name}"),
            )
        })?;

        if parsed.as_str() == LLM_CONFIG_BUNDLE_NAME {
            return Err(problem(
                400,
                "Bad Request",
                "Deletion of the default LLM config is not allowed",
            ));
        }

        if !is_builtin_config_bundle(parsed.as_str()) {
            catalog.bundle_config(&parsed).ok_or_else(|| {
                problem(
                    404,
                    "Not Found",
                    format!("Bundle '{bundle_name}' not found"),
                )
            })?;
        }

        config.delete(&parsed).await.map_err(config_err_500)?;

        if semiotic::is_bundle(parsed.as_str()) {
            semiotic::reset_hot_reload();
        }

        Ok(AxumStatus::NO_CONTENT)
    }
    .await;
    crate::metrics::finish_status_http_metrics("config_delete", start, &result);
    result
}

/// List version history (GET /config/{bundle_name}/versions).
#[utoipa::path(
    get,
    path = "/config/{bundle_name}/versions",
    tag = "config",
    security(("RunnerToken" = [])),
    params(("bundle_name" = String, Path, description = "Bundle name (e.g. llm or a tool bundle that has config)")),
    responses(
        (status = 200, description = "Version history", body = Vec<ConfigVersionDto>),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Bundle not found"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn list_config_versions(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path(bundle_name): axum::extract::Path<String>,
) -> HttpResult<Vec<ConfigVersionDto>> {
    let start = std::time::Instant::now();
    let result = async {
        let catalog = &state.tool_catalog;
        let config = &state.config_service;

        let parsed = BundleName::new(&bundle_name).map_err(|_| {
            problem(
                400,
                "Bad Request",
                format!("Invalid bundle name: {bundle_name}"),
            )
        })?;

        if !is_builtin_config_bundle(parsed.as_str()) {
            catalog.bundle_config(&parsed).ok_or_else(|| {
                problem(
                    404,
                    "Not Found",
                    format!("Bundle '{bundle_name}' not found"),
                )
            })?;
        }

        let versions = config
            .list_versions(&parsed)
            .await
            .map_err(config_err_500)?;

        let dtos: Vec<ConfigVersionDto> = versions
            .into_iter()
            .map(|v| ConfigVersionDto {
                version: v.version.into(),
                config: v.config,
                created_at_ms: v.created_at_ms.into(),
            })
            .collect();

        Ok(Json(dtos))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_list_versions", start, &result);
    result
}

/// Get config at specific version (GET /config/{bundle_name}/versions/{version}).
#[utoipa::path(
    get,
    path = "/config/{bundle_name}/versions/{version}",
    tag = "config",
    security(("RunnerToken" = [])),
    params(
        ("bundle_name" = String, Path, description = "Bundle name (e.g. llm or a tool bundle that has config)"),
        ("version" = u64, Path, description = "Version number")
    ),
    responses(
        (status = 200, description = "Config at version", body = ConfigVersionDto),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Bundle or version not found"),
        (status = 503, description = "Config service not available")
    )
)]
pub async fn get_config_version(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path((bundle_name, version)): axum::extract::Path<(String, u64)>,
) -> HttpResult<ConfigVersionDto> {
    let start = std::time::Instant::now();
    let result = async {
        let catalog = &state.tool_catalog;
        let config = &state.config_service;

        let parsed = BundleName::new(&bundle_name).map_err(|_| {
            problem(
                400,
                "Bad Request",
                format!("Invalid bundle name: {bundle_name}"),
            )
        })?;

        if !is_builtin_config_bundle(parsed.as_str()) {
            catalog.bundle_config(&parsed).ok_or_else(|| {
                problem(
                    404,
                    "Not Found",
                    format!("Bundle '{bundle_name}' not found"),
                )
            })?;
        }

        let v = config
            .get_version(&parsed, version)
            .await
            .map_err(config_err_500)?
            .ok_or_else(|| problem(404, "Not Found", format!("Version {version} not found")))?;

        Ok(Json(ConfigVersionDto {
            version: v.version.into(),
            config: v.config,
            created_at_ms: v.created_at_ms.into(),
        }))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_get_version", start, &result);
    result
}
