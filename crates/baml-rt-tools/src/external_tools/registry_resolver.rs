// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Cache-backed resolver for approved external-tool snapshots.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use baml_rt_core::{BamlRtError, Result};

use super::{
    ExternalLifecycleRecorder, ExternalToolSnapshot,
    discovery::{discover_snapshot, normalize_process_runtime},
    drift::DriftGuard,
    handler::ProcessToolHandler,
    metadata::{InvocationMode, build_tool_metadata, read_external_manifest},
    policy::{DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT},
    resolver::{SandboxRuntimeWiring, build_sandbox_tool_handler},
    runtime::{DEFAULT_PROCESS_COMMAND, ToolRuntime},
    snapshot::{
        compute_manifest_digest, compute_runtime_digest, now_snapshot_timestamp,
        validate_external_tool_snapshot,
    },
    snapshot_catalog::BUILDER_EXTERNAL_TOOL_CACHE_ENV,
    stdio::StdioSubprocessInvoker,
};
use crate::{
    ExternalToolResolver, ToolName,
    approval::ApprovalState,
    external_tool_cache,
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
        Self::from_cache_root_with_sandbox(root, None, None)
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
        recorder: Option<ExternalLifecycleRecorder>,
    ) -> Result<Self> {
        let snapshots = external_tool_cache::read_approved_snapshots(root)?;
        Self::from_snapshots(snapshots, sandbox, recorder)
    }

    /// Build a registry-backed resolver from trusted source directories.
    ///
    /// Each directory must contain `tool-manifest.json`. If an approved snapshot
    /// with matching manifest/runtime digests already exists under
    /// `snapshot_root`, it is reused without discovery. Missing or stale
    /// snapshots are discovered, approved, persisted, then loaded.
    pub async fn from_allowed_dirs(
        dirs: &[PathBuf],
        snapshot_root: &Path,
        sandbox: Option<SandboxRuntimeWiring>,
        recorder: Option<ExternalLifecycleRecorder>,
    ) -> Result<Self> {
        let mut snapshots = Vec::new();
        for dir in dirs {
            let mut manifest = read_external_manifest(dir)?;
            normalize_process_runtime(dir, &mut manifest);
            let tool_name = ToolName::parse(&manifest.name)?;
            let manifest_digest = compute_manifest_digest(&manifest);
            let runtime_digest = compute_runtime_digest(manifest.runtime.as_ref())?;
            let snapshot_path =
                external_tool_cache::approved_snapshot_path(snapshot_root, &manifest.name)?;

            let snapshot = if snapshot_path.is_file() {
                let existing = external_tool_cache::read_snapshot(&snapshot_path)?;
                if existing.tool.name == manifest.name
                    && existing.digests.manifest_digest == manifest_digest
                    && existing.digests.runtime_digest == runtime_digest
                    && existing.approval.state.is_approved()
                {
                    tracing::info!(tool = %tool_name, dir = %dir.display(), snapshot = %existing.snapshot_digest, "using approved external-tool snapshot from allowed dir");
                    existing
                } else {
                    tracing::info!(tool = %tool_name, dir = %dir.display(), "approved external-tool snapshot missing or stale; rediscovering");
                    discover_and_approve(dir, sandbox.as_ref()).await?
                }
            } else {
                tracing::info!(tool = %tool_name, dir = %dir.display(), "approved external-tool snapshot missing; discovering");
                discover_and_approve(dir, sandbox.as_ref()).await?
            };

            external_tool_cache::write_approved_snapshot(snapshot_root, &snapshot)?;
            snapshots.push(snapshot);
        }
        tracing::info!(count = snapshots.len(), root = %snapshot_root.display(), "loaded allowed external tool dirs");
        Self::from_snapshots(snapshots, sandbox, recorder)
    }

    pub fn from_snapshots(
        snapshots: Vec<ExternalToolSnapshot>,
        sandbox: Option<SandboxRuntimeWiring>,
        recorder: Option<ExternalLifecycleRecorder>,
    ) -> Result<Self> {
        let mut entries = std::collections::HashMap::new();
        for snapshot in snapshots {
            if !snapshot.approval.state.is_approved() {
                continue;
            }
            validate_external_tool_snapshot(&snapshot)?;
            let (name, entry) = load_snapshot(snapshot, sandbox.as_ref(), recorder.as_ref())?;
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

async fn discover_and_approve(
    dir: &Path,
    sandbox: Option<&SandboxRuntimeWiring>,
) -> Result<ExternalToolSnapshot> {
    let mut snapshot = discover_snapshot(dir, None, sandbox).await?;
    snapshot.approval.state = ApprovalState::Approved;
    snapshot.approval.reviewed_at = Some(now_snapshot_timestamp());
    Ok(snapshot)
}

fn load_snapshot(
    snapshot: ExternalToolSnapshot,
    sandbox: Option<&SandboxRuntimeWiring>,
    recorder: Option<&ExternalLifecycleRecorder>,
) -> Result<(ToolName, RegistryToolEntry)> {
    let tool_name = ToolName::parse(&snapshot.tool.name)?;
    // Registry/cache snapshots must be source-dir independent. Validation in
    // `from_snapshots` rejects coordination specs unless `coordination_baml` was
    // inlined at approval time, so an empty source path cannot silently drop a
    // referenced coordination file here.
    if matches!(snapshot.tool.invocation_mode, InvocationMode::Session)
        && !matches!(snapshot.tool.runtime, Some(ToolRuntime::Sandbox(_)))
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "registry-backed external tool '{}' declares invocation_mode=session but is not sandbox runtime; session mode is sandbox-only",
            tool_name
        )));
    }

    let mut metadata = build_tool_metadata(Path::new(""), &snapshot.tool, &tool_name)?;
    metadata.digest = Some(snapshot.digests.schema_digest.to_string());

    // Lazy first-invoke drift guard: the live tool's `tool/schema` is checked
    // against this approved digest on first use, then cached (mirrors the MCP
    // runtime tools-digest check). Drift fails the invoke closed.
    let drift_guard = Arc::new(DriftGuard::new(
        tool_name.clone(),
        snapshot.digests.schema_digest.to_string(),
        DEFAULT_DESCRIBE_TIMEOUT,
        recorder.cloned(),
    ));

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
                    .with_capabilities(snapshot.tool.capabilities.clone())
                    .with_drift_guard(drift_guard),
            )
        }
        ToolRuntime::Sandbox(_) => {
            let wiring = sandbox.ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "tool '{}' declares sandbox runtime but no sandbox wiring was provided to ExternalRegistryResolver",
                    tool_name
                ))
            })?;
            let spec_builder = (wiring.spec_factory)(&tool_name, &snapshot.tool)?;
            build_sandbox_tool_handler(
                metadata.clone(),
                &snapshot.tool,
                wiring,
                spec_builder,
                None,
                Some(drift_guard),
            )
        }
    };

    Ok((tool_name, RegistryToolEntry { metadata, handler }))
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use serde_json::json;

    use super::*;
    use crate::{
        ApprovalState,
        external_tools::{
            ExternalToolDescribeSnapshot, ExternalToolManifest, MetadataSchemas, SandboxImageRef,
            SandboxRuntimeSpec, ToolRuntime, ToolSchemaResult,
            sandbox::{MockSandboxProvider, SandboxCache, SandboxSpec},
        },
        tools::ToolAccess,
    };

    fn manifest(
        name: &str,
        runtime: Option<ToolRuntime>,
        mode: InvocationMode,
    ) -> ExternalToolManifest {
        let (bundle, local_name) = name.split_once('/').unwrap();
        ExternalToolManifest {
            tool_abi_version: "1".to_string(),
            name: name.to_string(),
            description: format!("{name} description"),
            bundle: bundle.to_string(),
            local_name: local_name.to_string(),
            access_level: ToolAccess::Read,
            tags: vec!["registry-test".to_string()],
            invocation_mode: mode,
            session_policy: Default::default(),
            secrets: vec![],
            secret_scope: Default::default(),
            capabilities: json!({}),
            config_bundle: None,
            runtime,
            coordination: None,
        }
    }

    fn snapshot(
        name: &str,
        runtime: Option<ToolRuntime>,
        mode: InvocationMode,
    ) -> ExternalToolSnapshot {
        let manifest = manifest(name, runtime, mode);
        let input = json!({"type": "object", "properties": {"q": {"type": "string"}}});
        let output = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}});
        let metadata = manifest.clone().into_metadata(MetadataSchemas {
            input: input.clone(),
            output: output.clone(),
        });
        let schema = ToolSchemaResult {
            schema_version: 1,
            tool_name: name.to_string(),
            content_type: "application/schema+json".to_string(),
            content_digest: crate::external_tools::compute_external_schema_digest(&metadata)
                .to_string(),
            input,
            output,
        };
        let describe = ExternalToolDescribeSnapshot {
            protocol_version: "1".to_string(),
            supported_methods: vec![crate::external_tools::METHOD_SCHEMA.to_string()],
            max_payload_bytes: None,
            schema_digest: None,
        };
        let mut snapshot = ExternalToolSnapshot::from_parts(
            Path::new(""),
            manifest,
            schema,
            describe,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        snapshot.approval.state = ApprovalState::Approved;
        snapshot
    }

    fn sandbox_runtime() -> ToolRuntime {
        ToolRuntime::Sandbox(SandboxRuntimeSpec {
            image: SandboxImageRef::Oci {
                r#ref: "example.com/tool@sha256:test".to_string(),
            },
            entrypoint: vec!["/tool-adapter".to_string()],
            adapter: None,
        })
    }

    fn sandbox_wiring() -> SandboxRuntimeWiring {
        SandboxRuntimeWiring {
            provider: Arc::new(MockSandboxProvider::echo()),
            cache: Arc::new(SandboxCache::new("registry-test-runner")),
            spec_factory: Arc::new(|tool_name, _meta| {
                let tool_name = tool_name.clone();
                Ok(Arc::new(move |key| {
                    Ok(SandboxSpec::for_test(
                        format!("sandbox:{}:{}", key.context_id, tool_name),
                        "example.com/tool:test",
                    ))
                }))
            }),
        }
    }

    #[test]
    fn approved_process_snapshot_resolves() {
        let snap = snapshot("support/registry_ok", None, InvocationMode::SingleShot);
        let resolver = ExternalRegistryResolver::from_snapshots(vec![snap], None, None).unwrap();
        let name = ToolName::parse("support/registry_ok").unwrap();
        let resolved = resolver.resolve(&name).unwrap();
        assert!(resolved.is_some());
        let (metadata, _) = resolved.unwrap();
        assert_eq!(metadata.name, name);
    }

    #[test]
    fn invalid_or_ambiguous_snapshots_reject() {
        let mut tampered = snapshot(
            "support/registry_tampered",
            None,
            InvocationMode::SingleShot,
        );
        tampered.tool.description = "tampered after approval".to_string();
        let err = match ExternalRegistryResolver::from_snapshots(vec![tampered], None, None) {
            Ok(_) => panic!("tampered snapshot should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("digest mismatch"),
            "unexpected: {err}"
        );

        let first = snapshot("support/registry_dup", None, InvocationMode::SingleShot);
        let second = snapshot("support/registry_dup", None, InvocationMode::SingleShot);
        let err = match ExternalRegistryResolver::from_snapshots(vec![first, second], None, None) {
            Ok(_) => panic!("duplicate snapshots should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("duplicate"), "unexpected: {err}");
    }

    #[test]
    fn sandbox_session_snapshot_requires_wiring_and_resolves_with_wiring() {
        let snap = snapshot(
            "support/registry_session",
            Some(sandbox_runtime()),
            InvocationMode::Session,
        );
        let err = match ExternalRegistryResolver::from_snapshots(vec![snap.clone()], None, None) {
            Ok(_) => panic!("sandbox snapshot without wiring should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("sandbox wiring"),
            "unexpected: {err}"
        );

        let resolver =
            ExternalRegistryResolver::from_snapshots(vec![snap], Some(sandbox_wiring()), None)
                .unwrap();
        let name = ToolName::parse("support/registry_session").unwrap();
        assert!(resolver.resolve(&name).unwrap().is_some());
    }
}
