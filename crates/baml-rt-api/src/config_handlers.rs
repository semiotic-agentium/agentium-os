//! Config and secret-request HTTP handlers.
//!
//! Config is keyed by bundle name; tools in a bundle share the same config.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode as AxumStatus};
use baml_rt_config::{InternalConfigReader, InternalConfigWriter};
use baml_rt_llm_config::{
    LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, LlmProvider, RuntimeSecretStore,
    SECRET_LINKS_CONFIG_KEY, SecretLinksState, SecretRequestName, SecretSourcePolicy, SecretValue,
    StoreKey, apply_secret_links_state,
};
use baml_rt_tools::{BundleName, ToolName};
use http_api_problem::HttpApiProblem;
use serde_json::Value;

use crate::openapi::{
    ConfigVersionDto, ProvisionSecretDto, SecretOverviewEntryDto, SecretRequestDto, ToolConfigDto,
    ToolConfigSchemaDto,
};

/// Minimal JSON Schema for LlmClientConfig so the config list and PUT validation can reference it.
fn llm_bundle_schema() -> Value {
    let provider_enum: Vec<Value> = LlmProvider::all()
        .iter()
        .map(|p| Value::String(p.as_str().to_string()))
        .collect();
    serde_json::json!({
        "type": "object",
        "properties": {
            "default": { "type": "string" },
            "clients": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "provider": { "type": "string", "enum": provider_enum },
                        "options": { "type": "object", "additionalProperties": { "type": "string" } },
                        "retry_policy": { "type": "string" }
                    },
                    "required": ["name", "provider"]
                }
            },
            "overrides": {
                "type": "object",
                "properties": {
                    "agent": { "type": "object", "additionalProperties": { "type": "string" } },
                    "agent_function": { "type": "object", "additionalProperties": { "type": "string" } }
                }
            },
            "retry_policies": { "type": "object" }
        },
        "required": ["default", "clients"]
    })
}

type HttpResult<T> = Result<Json<T>, HttpApiProblem>;

/// Build an HTTP problem. Invalid status codes are logged and replaced with 500.
fn problem(status: u16, title: &str, detail: impl Into<String>) -> HttpApiProblem {
    let detail = detail.into();
    match HttpApiProblem::try_new(status) {
        Ok(p) => p.title(title).detail(detail),
        Err(_) => {
            tracing::warn!(status, "invalid HTTP status in problem(); using 500");
            HttpApiProblem::try_new(500)
                .expect("500 is valid status")
                .title("Internal Error")
                .detail(detail)
        }
    }
}

/// Log config/store error and return 500 with a static client message (avoids leaking internal details).
fn config_err_500(e: impl std::fmt::Display) -> HttpApiProblem {
    tracing::error!(error = %e, "config operation failed");
    problem(500, "Internal Error", "Configuration operation failed")
}

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
fn secret_name_from_option_value(v: &str) -> Option<String> {
    let v = v.trim();
    v.strip_prefix("vault:")
        .or_else(|| v.strip_prefix("env."))
        .map(|s| s.to_string())
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

        catalog.bundle_config(&parsed).ok_or_else(|| {
            problem(
                404,
                "Not Found",
                format!("Bundle '{bundle_name}' not found"),
            )
        })?;

        config.delete(&parsed).await.map_err(config_err_500)?;

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

        if parsed.as_str() != LLM_CONFIG_BUNDLE_NAME {
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

        if parsed.as_str() != LLM_CONFIG_BUNDLE_NAME {
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
