//! Shared parsing for `tool-metadata.json`.
//!
//! Consumed by:
//! - `resolver::DevModeResolver` — runtime, needs metadata + handler.
//! - `metadata_catalog::ExternalMetadataCatalog` — build time, metadata only.
//!
//! Keeping the parsing in one place guarantees the runner and builder agree
//! on field semantics (the design doc calls this out as a hard invariant).

use std::path::Path;

use baml_rt_core::{BamlRtError, Result};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ToolName,
    tools::{
        BundleName, SecretRequest, SessionPolicy, ToolAccess, ToolBackend, ToolFunctionMetadata,
        ToolOrigin, ToolTypeSpec,
    },
};

/// Raw shape of `tool-metadata.json` (deserialized then projected into
/// [`ToolFunctionMetadata`] + [`SecretRequest`] + capability struct).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `bundle` / `local_name` are present for schema validation parity
pub(crate) struct RawToolMetadata {
    pub tool_abi_version: String,
    pub name: String,
    pub description: String,
    pub bundle: String,
    pub local_name: String,
    pub access_level: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub invocation_mode: String,
    #[serde(default)]
    pub session_policy: RawSessionPolicy,
    pub schemas: RawSchemas,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub config_bundle: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RawSessionPolicy {
    #[default]
    Strict,
    MultiSend,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawSchemas {
    pub input: Value,
    pub output: Value,
}

/// Read and minimally validate `<dir>/tool-metadata.json`.
pub(crate) fn read_raw_metadata(dir: &Path) -> Result<RawToolMetadata> {
    let metadata_path = dir.join("tool-metadata.json");
    let raw = std::fs::read_to_string(&metadata_path).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to read {}", metadata_path.display()),
            source: Box::new(e),
        }
    })?;
    let raw: RawToolMetadata =
        serde_json::from_str(&raw).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to parse {}", metadata_path.display()),
            source: Box::new(e),
        })?;

    if raw.tool_abi_version != "1" {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' declares unsupported ABI version '{}' (expected '1')",
            raw.name, raw.tool_abi_version
        )));
    }
    if raw.invocation_mode != "single_shot" {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' declares unsupported invocation_mode '{}' (expected 'single_shot')",
            raw.name, raw.invocation_mode
        )));
    }
    Ok(raw)
}

/// Project parsed metadata into the runtime [`ToolFunctionMetadata`] shape.
pub(crate) fn build_tool_metadata(
    raw: &RawToolMetadata,
    tool_name: &ToolName,
) -> Result<ToolFunctionMetadata> {
    let access = match raw.access_level.as_str() {
        "read" => Some(ToolAccess::Read),
        "write" => Some(ToolAccess::Write),
        "delete" => Some(ToolAccess::Delete),
        other => {
            return Err(BamlRtError::InvalidArgument(format!(
                "external tool '{}' has invalid access_level '{}'",
                tool_name, other
            )));
        }
    };

    let class_name = ToolFunctionMetadata::derive_class_name(tool_name.bundle(), tool_name.local());

    let config_bundle = match &raw.config_bundle {
        Some(s) => Some(BundleName::new(s)?),
        None => None,
    };

    let secret_requests: Vec<SecretRequest> = raw
        .secrets
        .iter()
        .map(|s| {
            SecretRequest::api_key(
                s.clone(),
                format!("Required by external tool {}", raw.name),
                s.clone(),
            )
        })
        .collect();

    Ok(ToolFunctionMetadata {
        name: tool_name.clone(),
        class_name: class_name.clone(),
        description: raw.description.clone(),
        // External tools have no "open input" concept in V1 — single-shot invoke.
        open_input_schema: serde_json::json!({}),
        input_schema: raw.schemas.input.clone(),
        output_schema: raw.schemas.output.clone(),
        open_input_type: ToolTypeSpec {
            name: "()".to_string(),
            ts_decl: None,
        },
        input_type: ToolTypeSpec {
            name: format!("{}Input", class_name),
            ts_decl: None,
        },
        output_type: ToolTypeSpec {
            name: format!("{}Output", class_name),
            ts_decl: None,
        },
        baml_decl: None,
        extra_ts_decls: Vec::new(),
        access,
        tags: raw.tags.clone(),
        secret_requests,
        config: None,
        config_bundle,
        origin: ToolOrigin::Host,
        backend: ToolBackend::ExternalProcess,
        projection_semantics: None,
        session_policy: match raw.session_policy {
            RawSessionPolicy::Strict => SessionPolicy::Strict,
            RawSessionPolicy::MultiSend => SessionPolicy::MultiSend,
        },
        event_sources: Vec::new(),
    })
}

/// Canonical SHA-256 of the input+output schemas. Both runner and tool author
/// must compute this identically for describe-mismatch detection to work.
pub(crate) fn metadata_schema_hash(raw: &RawToolMetadata) -> String {
    let payload = serde_json::json!({
        "input": sort_json_keys(&raw.schemas.input),
        "output": sort_json_keys(&raw.schemas.output),
    });
    let canonical = serde_json::to_string(&payload)
        .expect("serializing canonical tool schema payload should not fail");
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn sort_json_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by_key(|(ka, _)| *ka);
            let sorted = pairs
                .into_iter()
                .map(|(k, v)| (k.clone(), sort_json_keys(v)))
                .collect();
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(sort_json_keys).collect()),
        _ => value.clone(),
    }
}
