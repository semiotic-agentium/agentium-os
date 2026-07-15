// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared config handler utilities.

use axum::Json;
use baml_rt_llm_config::{LLM_CONFIG_BUNDLE_NAME, LlmProvider};
use http_api_problem::HttpApiProblem;
use serde_json::Value;

use super::semiotic;

pub(crate) type HttpResult<T> = Result<Json<T>, HttpApiProblem>;

pub(crate) fn is_builtin_config_bundle(name: &str) -> bool {
    name == LLM_CONFIG_BUNDLE_NAME || semiotic::is_bundle(name)
}

/// Build an HTTP problem. Invalid status codes are logged and replaced with 500.
pub(crate) fn problem(status: u16, title: &str, detail: impl Into<String>) -> HttpApiProblem {
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
pub(crate) fn config_err_500(e: impl std::fmt::Display) -> HttpApiProblem {
    tracing::error!(error = %e, "config operation failed");
    problem(500, "Internal Error", "Configuration operation failed")
}

pub(crate) fn llm_bundle_schema() -> Value {
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
            "retry_policies": { "type": "object" },
            "compaction": {
                "type": "object",
                "properties": {
                    "defaults": { "type": "object" },
                    "model_overrides": { "type": "object" },
                    "client_overrides": { "type": "object" },
                    "online_sources": { "type": "object" }
                }
            }
        },
        "required": ["default", "clients"]
    })
}
