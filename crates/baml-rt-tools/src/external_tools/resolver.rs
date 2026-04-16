//! Dev-mode local-filesystem resolver for external tools.
//!
//! Scans one or more "tool package" directories at construction time. Each dir
//! must contain:
//! - `tool-metadata.json` — matches `schemas/external_tool_metadata.schema.json`
//! - `tool-server`        — the executable (any stack that speaks the protocol)
//!
//! This resolver unblocks Phase 1 e2e without the OCI + lockfile pipeline.
//! Production deployments MUST use the digest-pinned lockfile resolver (Phase 2).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use baml_rt_core::{BamlRtError, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    ExternalToolResolver, ToolName,
    tools::{
        BundleName, SecretRequest, ToolAccess, ToolBackend, ToolFunctionMetadata, ToolHandler,
        ToolOrigin, ToolTypeSpec,
    },
};

use super::{
    handler::ProcessToolHandler,
    policy::{DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT},
    stdio::StdioSubprocessInvoker,
};

/// Raw shape of `tool-metadata.json` (deserialized then projected into
/// `ToolFunctionMetadata` + `SecretRequest` + capability struct).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `bundle` / `local_name` are read by schema validation; kept for completeness.
struct RawToolMetadata {
    tool_abi_version: String,
    name: String,
    description: String,
    bundle: String,
    local_name: String,
    access_level: String,
    #[serde(default)]
    tags: Vec<String>,
    invocation_mode: String,
    schemas: RawSchemas,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    capabilities: Value,
    #[serde(default)]
    config_bundle: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSchemas {
    input: Value,
    output: Value,
}

/// Local-filesystem resolver built from a set of tool package directories.
pub struct DevModeResolver {
    entries: HashMap<ToolName, (ToolFunctionMetadata, Arc<dyn ToolHandler>)>,
}

impl DevModeResolver {
    /// Load all tool packages from the supplied directories. Each directory
    /// must contain `tool-metadata.json` and a `tool-server` executable.
    pub fn from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut entries = HashMap::new();
        for dir in dirs {
            let (name, metadata, handler) = load_tool_dir(dir)?;
            if entries.contains_key(&name) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool '{}' loaded from {}",
                    name,
                    dir.display()
                )));
            }
            entries.insert(name, (metadata, handler));
        }
        Ok(Self { entries })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl ExternalToolResolver for DevModeResolver {
    fn resolve(
        &self,
        name: &ToolName,
    ) -> Result<Option<(ToolFunctionMetadata, Arc<dyn ToolHandler>)>> {
        Ok(self.entries.get(name).cloned())
    }
}

fn load_tool_dir(dir: &Path) -> Result<(ToolName, ToolFunctionMetadata, Arc<dyn ToolHandler>)> {
    let metadata_path = dir.join("tool-metadata.json");
    let bin_path = dir.join("tool-server");

    let raw = std::fs::read_to_string(&metadata_path).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to read {}", metadata_path.display()),
            source: Box::new(e),
        }
    })?;
    let raw: RawToolMetadata = serde_json::from_str(&raw).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to parse {}", metadata_path.display()),
            source: Box::new(e),
        }
    })?;

    // Minimum sanity checks — the JSON Schema would enforce the rest.
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
    if !bin_path.exists() {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool binary not found at {}",
            bin_path.display()
        )));
    }

    let tool_name = ToolName::parse(&raw.name)?;
    let metadata = build_metadata(&raw, &tool_name)?;

    let invoker = Arc::new(StdioSubprocessInvoker::new(bin_path));
    let handler: Arc<dyn ToolHandler> = Arc::new(
        ProcessToolHandler::new(metadata.clone(), invoker, DEFAULT_INVOKE_TIMEOUT)
            .with_capabilities(raw.capabilities.clone()),
    );

    // Silence unused-const warning until Phase 1.7 wires describe at load.
    let _ = DEFAULT_DESCRIBE_TIMEOUT;

    Ok((tool_name, metadata, handler))
}

fn build_metadata(raw: &RawToolMetadata, tool_name: &ToolName) -> Result<ToolFunctionMetadata> {
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

    let class_name =
        ToolFunctionMetadata::derive_class_name(tool_name.bundle(), tool_name.local());

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
        session_policy: Default::default(),
        event_sources: Vec::new(),
    })
}
