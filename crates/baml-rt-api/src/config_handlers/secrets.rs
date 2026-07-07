// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Secret store and link handlers.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode as AxumStatus};
use baml_rt_config::{InternalConfigReader, InternalConfigWriter};
use baml_rt_llm_config::{
    LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, RuntimeSecretStore, SECRET_LINKS_CONFIG_KEY,
    SecretLinksState, SecretRequestName, SecretSourcePolicy, SecretValue, StoreKey,
    apply_secret_links_state, strip_placeholder_prefix,
};
use baml_rt_tools::{BundleName, ToolName};
use http_api_problem::HttpApiProblem;

use super::common::{HttpResult, config_err_500, problem};
use crate::openapi::{ProvisionSecretDto, SecretOverviewEntryDto, SecretRequestDto};

async fn load_secret_links_state(
    config_service: &dyn InternalConfigReader,
) -> Result<SecretLinksState, Box<HttpApiProblem>> {
    let opt = config_service
        .get_internal(SECRET_LINKS_CONFIG_KEY)
        .await
        .map_err(|e| Box::new(config_err_500(e)))?;
    let state: SecretLinksState = opt.map_or_else(SecretLinksState::default, |v| {
        serde_json::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "secret link state deserialize failed; using default");
            SecretLinksState::default()
        })
    });
    Ok(state)
}

async fn save_secret_links_state(
    config_service: &dyn InternalConfigWriter,
    state: &SecretLinksState,
) -> Result<(), Box<HttpApiProblem>> {
    let value = serde_json::to_value(state).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize secret links state");
        Box::new(problem(500, "Internal Error", "Serialization failure"))
    })?;
    config_service
        .set_internal(SECRET_LINKS_CONFIG_KEY, value)
        .await
        .map_err(|e| Box::new(config_err_500(e)))?;
    Ok(())
}

/// Reload the persisted secret-link state from the shared config store and
/// apply it to the local overlay. Call before any handler that reads or
/// mutates secret links so the overlay reflects changes made by other runners.
async fn sync_secret_links(state: &crate::router::ApiState) {
    let Some(store) = state.runtime_secret_store.as_ref() else {
        return;
    };
    let link_state = match load_secret_links_state(state.config_service.as_ref()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "sync_secret_links: failed to load shared link state");
            return;
        }
    };
    apply_secret_links_state(
        &link_state,
        store.as_ref() as &dyn RuntimeSecretStore,
        state.secret_resolver.as_ref(),
    );
}

/// Extract secret key from an option value (placeholder prefix stripped; key used for lookup).
/// Returns `None` when the value carries no recognised placeholder prefix.
fn secret_name_from_option_value(v: &str) -> Option<String> {
    strip_placeholder_prefix(v).map(str::to_string)
}

/// List all required secrets with M:N consumers: tools and LLM clients (GET /config/secrets-overview).
#[utoipa::path(
    get,
    path = "/config/secrets-overview",
    tag = "config",
    security(("RunnerToken" = [])),
    responses(
        (status = 200, description = "Secrets and their tool/LLM consumers", body = Vec<SecretOverviewEntryDto>),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 503, description = "Tool catalog not available")
    )
)]
pub async fn list_secrets_overview(
    State(state): State<Arc<crate::router::ApiState>>,
) -> HttpResult<Vec<SecretOverviewEntryDto>> {
    let start = std::time::Instant::now();
    sync_secret_links(&state).await;
    let result = async {
    let catalog = &state.tool_catalog;
    // LLM_CONFIG_BUNDLE_NAME is a crate constant guaranteed by baml_rt_llm_config to pass BundleName::new.
    let llm_bundle = BundleName::new(LLM_CONFIG_BUNDLE_NAME).expect("llm bundle name valid");
    let llm_config = match state
        .config_service
        .get_with_version(&llm_bundle)
        .await
        .map_err(config_err_500)?
    {
        Some(s) => match LlmClientConfig::from_value(s.config) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "stored LLM config parse failed; using sensible default");
                LlmClientConfig::sensible_default()
            }
        },
        None => LlmClientConfig::sensible_default(),
    };

    #[derive(Default)]
    struct Entry {
        secret_type: Option<String>,
        justification: Option<String>,
        descriptor: Option<String>,
        tool_consumers: Vec<String>,
        llm_consumers: Vec<String>,
    }
    let mut by_name: std::collections::HashMap<String, Entry> = std::collections::HashMap::new();

    for metadata in catalog.iter() {
        let tool_name = metadata.name.to_string();
        for sr in &metadata.secret_requests {
            let e = by_name.entry(sr.name.clone()).or_default();
            if e.secret_type.is_none() {
                e.secret_type = Some(sr.secret_type.as_str().to_string());
                e.justification = Some(sr.justification.clone());
                e.descriptor = Some(sr.descriptor.clone());
            }
            if !e.tool_consumers.contains(&tool_name) {
                e.tool_consumers.push(tool_name.clone());
            }
        }
    }

    for (client_name, client) in &llm_config.clients {
        for opt_value in client.options.values() {
            if let Some(secret_name) = secret_name_from_option_value(opt_value) {
                let e = by_name.entry(secret_name).or_default();
                if !e.llm_consumers.contains(client_name) {
                    e.llm_consumers.push(client_name.clone());
                }
            }
        }
    }

    let resolver = &state.secret_resolver;
    let link_state = match load_secret_links_state(state.config_service.as_ref()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "secret link state load failed; reporting as empty");
            SecretLinksState::default()
        }
    };
    let mut out: Vec<SecretOverviewEntryDto> = by_name
        .into_iter()
        .map(|(name, e)| {
            // Satisfied only when explicitly linked: resolver returns a non-empty value (not just key presence).
            let satisfied = resolver
                .resolve(name.as_str())
                .map(|v| !v.as_str().trim().is_empty())
                .unwrap_or(false);
            let request = SecretRequestName::new(name.as_str());
            let linked_to = link_state
                .links
                .get(&request)
                .map(|k| k.as_str().to_string());
            SecretOverviewEntryDto {
                name,
                secret_type: e.secret_type,
                justification: e.justification,
                descriptor: e.descriptor,
                tool_consumers: e.tool_consumers,
                llm_consumers: e.llm_consumers,
                satisfied,
                linked_to,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(out))
    }.await;
    crate::metrics::finish_json_http_metrics("config_secrets_overview", start, &result);
    result
}

/// List keys in the secret store that have a value (for link dropdown). Returns only keys that the backend (e.g. fnox) actually resolved — not secret request names from the overview. M:N: each secret request is linked to one of these store keys.
#[utoipa::path(
    get,
    path = "/config/secrets/store-keys",
    tag = "config",
    security(("RunnerToken" = [])),
    responses(
        (status = 200, description = "Store key names that can be used as link_from", body = Vec<String>),
        (status = 401, description = "Missing or invalid runner token")
    )
)]
pub async fn list_store_keys(
    State(state): State<Arc<crate::router::ApiState>>,
) -> HttpResult<Vec<String>> {
    let start = std::time::Instant::now();
    let result = async {
        let keys = state
            .secret_resolver
            .list_store_keys()
            .into_iter()
            .map(|k| k.as_str().to_string())
            .collect();
        Ok(Json(keys))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_store_keys", start, &result);
    result
}

/// Link a secret by name (PUT /config/secrets/{name}).
/// Copies the value of `link_from` from the secret store (fnox) into the runtime overlay for `name`. No raw values accepted.
#[utoipa::path(
    put,
    path = "/config/secrets/{name}",
    tag = "config",
    security(("RunnerToken" = [])),
    params(("name" = String, Path, description = "Secret key to link (e.g. NOTION_API_TOKEN)")),
    request_body = ProvisionSecretDto,
    responses(
        (status = 204, description = "Secret linked"),
        (status = 400, description = "Invalid name or link_from has no value in store"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 501, description = "Runtime secret store not available (add keys to fnox and restart)")
    )
)]
pub async fn put_secret(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(body): Json<ProvisionSecretDto>,
) -> Result<AxumStatus, HttpApiProblem> {
    let start = std::time::Instant::now();
    sync_secret_links(&state).await;
    let result = async {
    let store = state
        .runtime_secret_store
        .as_ref()
        .ok_or_else(|| {
            problem(
                501,
                "Not Implemented",
                "Runtime secret store not available. Provision secrets via your secret store (e.g. env or fnox).",
            )
        })?;
    let name = name.trim();
    if name.is_empty() {
        return Err(problem(400, "Bad Request", "Secret name must be non-empty"));
    }
    let link_from = body.link_from.trim();
    if link_from.is_empty() {
        return Err(problem(400, "Bad Request", "link_from must be non-empty"));
    }
    let request_name = SecretRequestName::new(name);
    let store_key = StoreKey::new(link_from);
    let env_hint = match SecretSourcePolicy::from_env() {
        SecretSourcePolicy::FnoxOnly => "",
        SecretSourcePolicy::FnoxWithEnvFallback => " (or set it as an environment variable)",
    };
    let value = state
        .secret_resolver
        .resolve_from_store(&store_key)
        .filter(|v| !v.as_str().trim().is_empty())
        .ok_or_else(|| {
            problem(
                400,
                "Bad Request",
                format!(
                    "Key '{link_from}' has no value in the secret store. \
                     Add it to fnox.toml{env_hint} and restart the runner."
                ),
            )
        })?
        .into_string();
    store.set(&request_name, SecretValue::new(value));
    let mut link_state = load_secret_links_state(state.config_service.as_ref())
        .await
        .map_err(|e| *e)?;
    link_state.links.insert(request_name, store_key);
    link_state.unlinked.retain(|r| r.as_str() != name);
    save_secret_links_state(state.config_service.as_ref(), &link_state)
        .await
        .map_err(|e| *e)?;
    Ok(AxumStatus::NO_CONTENT)
    }.await;
    crate::metrics::finish_status_http_metrics("config_secret_put", start, &result);
    result
}

/// Unlink a secret (DELETE /config/secrets/{name}).
/// Removes the key from the runtime overlay; resolution then falls back to the secret store (fnox). Returns 501 when store not available.
#[utoipa::path(
    delete,
    path = "/config/secrets/{name}",
    tag = "config",
    security(("RunnerToken" = [])),
    params(("name" = String, Path, description = "Secret key to unlink (e.g. NOTION_API_TOKEN)")),
    responses(
        (status = 204, description = "Secret unlinked"),
        (status = 400, description = "Invalid name"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 501, description = "Runtime secret store not available")
    )
)]
pub async fn delete_secret(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<AxumStatus, HttpApiProblem> {
    let start = std::time::Instant::now();
    sync_secret_links(&state).await;
    let result = async {
        let store = state.runtime_secret_store.as_ref().ok_or_else(|| {
            problem(
                501,
                "Not Implemented",
                "Runtime secret store not available.",
            )
        })?;
        let name = name.trim();
        if name.is_empty() {
            return Err(problem(400, "Bad Request", "Secret name must be non-empty"));
        }
        let request = SecretRequestName::new(name);
        store.remove(&request);
        let mut link_state = load_secret_links_state(state.config_service.as_ref())
            .await
            .map_err(|e| *e)?;
        link_state.links.remove(&request);
        if !link_state.unlinked.iter().any(|r| r.as_str() == name) {
            link_state.unlinked.push(request);
        }
        save_secret_links_state(state.config_service.as_ref(), &link_state)
            .await
            .map_err(|e| *e)?;
        Ok(AxumStatus::NO_CONTENT)
    }
    .await;
    crate::metrics::finish_status_http_metrics("config_secret_delete", start, &result);
    result
}

/// List secret requests for a tool (GET /config/{tool_name}/secret-requests).
#[utoipa::path(
    get,
    path = "/config/{tool_name}/secret-requests",
    tag = "config",
    security(("RunnerToken" = [])),
    params(("tool_name" = String, Path, description = "Tool name (bundle/local)")),
    responses(
        (status = 200, description = "Secret requests", body = Vec<SecretRequestDto>),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Tool not found"),
        (status = 503, description = "Tool catalog not available")
    )
)]
pub async fn list_secret_requests(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path(tool_name): axum::extract::Path<String>,
) -> HttpResult<Vec<SecretRequestDto>> {
    let start = std::time::Instant::now();
    let result = async {
        let catalog = &state.tool_catalog;

        let parsed = ToolName::parse(&tool_name).map_err(|_| {
            problem(
                400,
                "Bad Request",
                format!("Invalid tool name: {tool_name}"),
            )
        })?;

        let metadata = catalog
            .by_name(&parsed)
            .ok_or_else(|| problem(404, "Not Found", format!("Tool '{tool_name}' not found")))?;

        let dtos: Vec<SecretRequestDto> = metadata
            .secret_requests
            .iter()
            .map(|sr| SecretRequestDto {
                name: sr.name.clone(),
                secret_type: sr.secret_type.as_str().to_string(),
                justification: sr.justification.clone(),
                descriptor: sr.descriptor.clone(),
            })
            .collect();

        Ok(Json(dtos))
    }
    .await;
    crate::metrics::finish_json_http_metrics("config_secret_requests", start, &result);
    result
}
