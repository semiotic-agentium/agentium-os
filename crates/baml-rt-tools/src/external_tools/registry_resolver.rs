// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Cache-backed resolver for approved external-tool snapshots.

use std::{path::Path, sync::Arc};

use baml_rt_core::{BamlRtError, Result};

use super::{
    ExternalToolSnapshot,
    handler::ProcessToolHandler,
    metadata::build_tool_metadata,
    policy::DEFAULT_INVOKE_TIMEOUT,
    resolver::SandboxRuntimeWiring,
    runtime::{DEFAULT_PROCESS_COMMAND, ToolRuntime},
    sandbox::{SandboxSpecBuilder, SandboxToolHandler},
    snapshot::validate_external_tool_snapshot,
    snapshot_catalog::BUILDER_EXTERNAL_TOOL_CACHE_ENV,
    stdio::StdioSubprocessInvoker,
};
use crate::{
    ExternalToolResolver, ToolName, external_tool_cache,
    tools::{ToolFunctionMetadata, ToolHandler},
};

pub struct ExternalRegistryResolver {
    entries: std::collections::HashMap<ToolName, RegistryToolEntry>,
}

struct RegistryToolEntry {
    metadata: ToolFunctionMetadata,
    handler: Arc<dyn ToolHandler>,
}

impl ExternalRegistryResolver {
    pub fn from_cache_root(root: &Path) -> Result<Self> {
        Self::from_cache_root_with_sandbox(root, None)
    }

    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var(BUILDER_EXTERNAL_TOOL_CACHE_ENV) {
            Ok(value) if !value.trim().is_empty() => {
                Self::from_cache_root(Path::new(&value)).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub fn from_cache_root_with_sandbox(
        root: &Path,
        sandbox: Option<SandboxRuntimeWiring>,
    ) -> Result<Self> {
        let snapshots = external_tool_cache::read_approved_snapshots(root)?;
        Self::from_snapshots(snapshots, sandbox)
    }

    pub fn from_snapshots(
        snapshots: Vec<ExternalToolSnapshot>,
        sandbox: Option<SandboxRuntimeWiring>,
    ) -> Result<Self> {
        let mut entries = std::collections::HashMap::new();
        for snapshot in snapshots {
            if !snapshot.approval.state.is_approved() {
                continue;
            }
            validate_external_tool_snapshot(&snapshot)?;
            let (name, entry) = load_snapshot(snapshot, sandbox.as_ref())?;
            if entries.insert(name.clone(), entry).is_some() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool snapshot '{}'",
                    name
                )));
            }
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

impl ExternalToolResolver for ExternalRegistryResolver {
    fn resolve(
        &self,
        name: &ToolName,
    ) -> Result<Option<(ToolFunctionMetadata, Arc<dyn ToolHandler>)>> {
        let Some(entry) = self.entries.get(name) else {
            return Ok(None);
        };
        Ok(Some((entry.metadata.clone(), entry.handler.clone())))
    }
}

fn load_snapshot(
    snapshot: ExternalToolSnapshot,
    sandbox: Option<&SandboxRuntimeWiring>,
) -> Result<(ToolName, RegistryToolEntry)> {
    let tool_name = ToolName::parse(&snapshot.tool.name)?;
    // Registry/cache snapshots must be source-dir independent. Validation in
    // `from_snapshots` rejects coordination specs unless `coordination_baml` was
    // inlined at approval time, so an empty source path cannot silently drop a
    // referenced coordination file here.
    let mut metadata = build_tool_metadata(Path::new(""), &snapshot.tool, &tool_name)?;
    metadata.digest = Some(snapshot.digests.schema_digest.to_string());

    let runtime = snapshot.tool.runtime.clone().unwrap_or_default();
    let handler: Arc<dyn ToolHandler> = match runtime {
        ToolRuntime::Process(spec) => {
            let command = if spec.command.is_empty() {
                vec![DEFAULT_PROCESS_COMMAND.to_string()]
            } else {
                spec.command
            };
            let invoker = Arc::new(StdioSubprocessInvoker::from_command(command)?);
            Arc::new(
                ProcessToolHandler::new(metadata.clone(), invoker, DEFAULT_INVOKE_TIMEOUT)
                    .with_capabilities(snapshot.tool.capabilities.clone()),
            )
        }
        ToolRuntime::Sandbox(_) => {
            let wiring = sandbox.ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "tool '{}' declares sandbox runtime but no sandbox wiring was provided to ExternalRegistryResolver",
                    tool_name
                ))
            })?;
            let spec_builder: SandboxSpecBuilder =
                (wiring.spec_factory)(&tool_name, &snapshot.tool)?;
            Arc::new(
                SandboxToolHandler::new(
                    metadata.clone(),
                    wiring.provider.clone(),
                    wiring.cache.clone(),
                    spec_builder,
                    DEFAULT_INVOKE_TIMEOUT,
                )
                .with_capabilities(snapshot.tool.capabilities.clone()),
            )
        }
    };

    Ok((tool_name, RegistryToolEntry { metadata, handler }))
}
