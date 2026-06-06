// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::Path};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    metadata::{ExternalToolMetadata, InvocationMode},
    protocol::{METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA, PROTOCOL_VERSION},
    runtime::ToolRuntime,
};

pub const SIDECAR_DIR_ABS: &str = "/etc/agentium";
pub const SIDECAR_BUNDLE_ABS_PATH: &str = "/etc/agentium/tool-bundle.json";
pub const SIDECAR_BUNDLE_REL_PATH: &str = "etc/agentium/tool-bundle.json";
pub const DEFAULT_SCHEMA_CONTENT_TYPE: &str = "application/schema+json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSidecarBundle {
    pub runtime: ToolRuntimeSidecar,
    pub manifest: ToolManifestSidecar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRuntimeSidecar {
    pub schema_version: u32,
    pub tool_id: String,
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifestSidecar {
    pub tool_name: String,
    pub protocol_version: String,
    pub supported_methods: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchemaSidecar {
    pub schema_version: u64,
    pub tool_name: String,
    pub content_type: String,
    pub content_digest: String,
    pub input: Value,
    pub output: Value,
}

pub fn render_sidecar_bundle(meta: &ExternalToolMetadata) -> Result<ToolSidecarBundle> {
    let ToolRuntime::Sandbox(sandbox) = meta.runtime.clone().ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "sidecar bundle generation requires runtime.kind=sandbox (tool: {})",
            meta.name
        ))
    })?
    else {
        return Err(BamlRtError::InvalidArgument(format!(
            "sidecar bundle generation requires runtime.kind=sandbox (tool: {})",
            meta.name
        )));
    };

    let adapter = sandbox.adapter.ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "sandbox runtime requires runtime.adapter to generate sidecar bundle (tool: {})",
            meta.name
        ))
    })?;

    Ok(ToolSidecarBundle {
        runtime: ToolRuntimeSidecar {
            schema_version: adapter.schema_version,
            tool_id: meta.name.clone(),
            command: adapter.command,
            workdir: adapter.workdir,
            protocol: adapter.protocol,
        },
        manifest: ToolManifestSidecar {
            tool_name: meta.name.clone(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            supported_methods: match meta.invocation_mode {
                InvocationMode::SingleShot => [METHOD_DESCRIBE, METHOD_SCHEMA, METHOD_INVOKE]
                    .iter()
                    .map(|m| (*m).to_string())
                    .collect(),
                InvocationMode::Session => {
                    baml_sandbox_protocol::session::SUPPORTED_METHODS_SESSION
                        .iter()
                        .map(|m| (*m).to_string())
                        .collect()
                }
            },
        },
    })
}

pub fn read_sidecar_bundle(path: &Path) -> Result<ToolSidecarBundle> {
    let raw = fs::read_to_string(path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
        message: format!("failed to read sidecar bundle {}", path.display()),
        source: Box::new(e),
    })?;
    serde_json::from_str::<ToolSidecarBundle>(&raw).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to parse sidecar bundle {}", path.display()),
            source: Box::new(e),
        }
    })
}
