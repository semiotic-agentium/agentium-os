// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External-tool discovery helpers.
//!
//! Discovery reads the authored `tool-manifest.json`, runs the tool protocol
//! (`tool/describe` + `tool/schema`), and assembles an approved-cache-ready
//! snapshot. It intentionally does not approve or persist anything.

use std::{path::Path, time::Duration};

use baml_rt_core::{
    BamlRtError, ContextId, Result,
    ids::{AgentId, UuidId},
};

use super::{
    ExternalToolManifest, METHOD_INVOKE, MetadataSchemas, PROTOCOL_VERSION, SandboxImageRef,
    StdioSubprocessInvoker, ToolDescribeResult, ToolInvoker, ToolRuntime,
    metadata::InvocationMode,
    now_snapshot_timestamp, read_external_manifest,
    resolver::SandboxRuntimeWiring,
    sandbox::{SandboxCacheKey, SandboxInvoker},
    snapshot::{ExternalToolSnapshot, validate_describe_schema_support},
};
use crate::ToolName;

pub const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Discover an external tool snapshot from an authored tool directory.
///
/// `dir` must contain `tool-manifest.json`. Process tools are launched via
/// stdio with `dir` as working directory. Sandbox tools require sandbox wiring;
/// `sandbox_rootfs` can override the manifest image with a bind rootfs.
pub async fn discover_snapshot(
    dir: &Path,
    sandbox_rootfs: Option<std::path::PathBuf>,
    sandbox: Option<&SandboxRuntimeWiring>,
) -> Result<ExternalToolSnapshot> {
    let mut manifest = read_external_manifest(dir).map_err(|err| {
        BamlRtError::InvalidArgument(format!(
            "reading external tool manifest from {} failed: {err}",
            dir.display()
        ))
    })?;
    if let Some(rootfs) = sandbox_rootfs {
        match manifest.runtime.as_mut() {
            Some(ToolRuntime::Sandbox(spec)) => {
                spec.image = SandboxImageRef::Bind { path: rootfs };
            }
            Some(ToolRuntime::Process(_)) | None => {
                return Err(BamlRtError::InvalidArgument(
                    "--sandbox-rootfs was provided but tool runtime is process".to_string(),
                ));
            }
        }
    }

    normalize_process_runtime(dir, &mut manifest);

    let tool_name = ToolName::parse(&manifest.name)?;
    match manifest.runtime.clone().unwrap_or_default() {
        ToolRuntime::Process(spec) => {
            let invoker = process_invoker(dir, spec.command)?;
            discover_via_invoker(&invoker, dir, manifest, &tool_name).await
        }
        ToolRuntime::Sandbox(_) => {
            let wiring = sandbox.ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "tool '{}' declares sandbox runtime but runner has no sandbox provider/wiring enabled",
                    manifest.name
                ))
            })?;
            let metadata = manifest.clone().into_metadata(MetadataSchemas {
                input: serde_json::json!({}),
                output: serde_json::json!({}),
            });
            let spec_builder = (wiring.spec_factory)(&tool_name, &metadata)?;
            // Sentinel scope — discovery is not a real agent/context. Bind ids
            // so we can evict + tear down exact cache entry afterwards.
            let agent_id = AgentId::from_uuid(
                UuidId::parse_str("00000000-0000-0000-0000-000000000001")
                    .expect("static discovery uuid"),
            );
            let context_id = ContextId::new(0, 1);
            let invoker = SandboxInvoker::new(
                wiring.provider.clone(),
                wiring.cache.clone(),
                spec_builder,
                agent_id.clone(),
                context_id.clone(),
            )
            .with_timeouts(DEFAULT_DISCOVERY_TIMEOUT, DEFAULT_DISCOVERY_TIMEOUT);
            let result = discover_via_invoker(&invoker, dir, manifest, &tool_name).await;
            // Discovery is one-shot: avoid reusing stale schema sandbox.
            let key = SandboxCacheKey {
                agent_id,
                context_id,
                tool_name: tool_name.clone(),
            };
            if let Some(handle) = wiring.cache.evict(&key)
                && let Err(err) = wiring.provider.teardown(&handle).await
            {
                tracing::warn!(error = %err, tool = %tool_name, "failed to tear down discovery sandbox");
            }
            result
        }
    }
}

async fn discover_via_invoker(
    invoker: &dyn ToolInvoker,
    dir: &Path,
    manifest: ExternalToolManifest,
    tool_name: &ToolName,
) -> Result<ExternalToolSnapshot> {
    let describe = invoker
        .describe(tool_name, DEFAULT_DISCOVERY_TIMEOUT)
        .await?;
    let describe_result: ToolDescribeResult = describe.into();
    validate_describe(&manifest.name, manifest.invocation_mode, &describe_result)?;
    let describe_snapshot = validate_describe_schema_support(&manifest.name, &describe_result)?;
    let schema = invoker.schema(tool_name, DEFAULT_DISCOVERY_TIMEOUT).await?;
    ExternalToolSnapshot::from_parts(
        dir,
        manifest,
        schema,
        describe_snapshot,
        now_snapshot_timestamp(),
    )
}

fn validate_describe(
    tool: &str,
    invocation_mode: InvocationMode,
    describe: &ToolDescribeResult,
) -> Result<()> {
    if describe.protocol_version != PROTOCOL_VERSION {
        return Err(BamlRtError::InvalidArgument(format!(
            "tool/describe protocol_version '{}' but expected '{}'",
            describe.protocol_version, PROTOCOL_VERSION
        )));
    }

    match invocation_mode {
        InvocationMode::SingleShot => {
            if !describe
                .supported_methods
                .iter()
                .any(|m| m == METHOD_INVOKE)
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "tool '{}' does not advertise {}",
                    tool, METHOD_INVOKE
                )));
            }
        }
        InvocationMode::Session => {
            for required in baml_sandbox_protocol::session::SUPPORTED_METHODS_SESSION {
                if !describe
                    .supported_methods
                    .iter()
                    .any(|method| method == required)
                {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "session tool '{}' does not advertise {}",
                        tool, required
                    )));
                }
            }
        }
    }

    Ok(())
}

pub(super) fn normalize_process_runtime(dir: &Path, manifest: &mut ExternalToolManifest) {
    let ToolRuntime::Process(mut spec) = manifest.runtime.clone().unwrap_or_default() else {
        return;
    };
    if spec.command.is_empty() {
        spec.command = vec![super::DEFAULT_PROCESS_COMMAND.to_string()];
    }
    if let Some(first) = spec.command.first_mut() {
        let path = std::path::PathBuf::from(&first);
        if path.is_relative() {
            *first = dir.join(path).to_string_lossy().to_string();
        }
    }
    manifest.runtime = Some(ToolRuntime::Process(spec));
}

fn process_invoker(dir: &Path, command: Vec<String>) -> Result<StdioSubprocessInvoker> {
    let mut command = if command.is_empty() {
        vec![super::DEFAULT_PROCESS_COMMAND.to_string()]
    } else {
        command
    };
    if let Some(first) = command.first_mut() {
        let path = std::path::PathBuf::from(&first);
        if path.is_relative() {
            *first = dir.join(path).to_string_lossy().to_string();
        }
    }
    Ok(StdioSubprocessInvoker::from_command(command)?.with_working_dir(dir.to_path_buf()))
}
