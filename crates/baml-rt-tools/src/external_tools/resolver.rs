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
    sync::{Arc, Mutex, OnceLock},
    time::UNIX_EPOCH,
};

use baml_rt_core::{BamlRtError, Result};

use super::{
    ExternalLifecycleEvent, ExternalLifecycleRecorder, ExternalSessionToolHandler,
    handler::ProcessToolHandler,
    invoker::{ExternalInvoker, ToolDescribe},
    lockfile::{ExternalLockfileMode, ExternalToolsLockfile},
    metadata::{
        ExternalToolMetadata, InvocationMode, build_tool_metadata, compute_tool_digest,
        metadata_schema_digest, read_runtime_external_metadata,
    },
    policy::{DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT},
    protocol::{METHOD_INVOKE, PROTOCOL_VERSION},
    runtime::ToolRuntime,
    sandbox::{
        SandboxCache, SandboxProvider, SandboxSessionInvoker, SandboxSessionInvokerConfig,
        SandboxSpecBuilder, SandboxToolHandler, SessionPool, SessionPoolConfig,
    },
    stdio::StdioSubprocessInvoker,
};
use crate::{
    ExternalToolResolver, ToolName,
    tools::{ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};

/// Per-tool callback the resolver invokes to build a
/// [`SandboxSpecBuilder`] from parsed metadata. Workstream D plugs in
/// policy compilation, secret resolution, and runtime-digest selection
/// behind this type.
pub type SandboxSpecFactory = Arc<
    dyn Fn(&ToolName, &ExternalToolMetadata) -> Result<SandboxSpecBuilder> + Send + Sync + 'static,
>;

/// Plumbing the runner passes in when it wants sandbox-declared tools to be
/// routed through [`SandboxToolHandler`] at resolve time. Without this
/// wiring, a tool whose metadata declares `runtime.kind = "sandbox"` is
/// rejected with `sandbox runtime not wired`.
///
/// Ownership model: the runner keeps one provider + cache per process
/// (§9.2 `runner_id`).
pub struct SandboxRuntimeWiring {
    pub provider: Arc<dyn SandboxProvider>,
    pub cache: Arc<SandboxCache>,
    pub spec_factory: SandboxSpecFactory,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct DescribeCacheKey {
    tool_name: String,
    identity: String,
}

static DESCRIBE_CACHE: OnceLock<Mutex<HashMap<DescribeCacheKey, ToolDescribe>>> = OnceLock::new();

/// Local-filesystem resolver built from a set of tool package directories.
pub struct DevModeResolver {
    entries: HashMap<ToolName, DevToolEntry>,
    lockfile: Option<ExternalToolsLockfile>,
    lockfile_mode: ExternalLockfileMode,
    lifecycle_recorder: Option<ExternalLifecycleRecorder>,
}

#[derive(Clone)]
struct DevToolEntry {
    metadata: ToolFunctionMetadata,
    handler: Arc<dyn ToolHandler>,
    digest: String,
    artifact_ref: String,
}

impl DevModeResolver {
    /// Load all tool packages from the supplied directories. Each directory
    /// must contain `tool-metadata.json` and a `tool-server` executable.
    pub async fn from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        Self::from_dirs_with_policy(dirs, None, ExternalLockfileMode::Off, None).await
    }

    /// Same as [`Self::from_dirs`] but emits lifecycle callbacks for external-tool
    /// describe/artifact operations when a recorder is provided.
    pub async fn from_dirs_with_lifecycle(
        dirs: &[PathBuf],
        lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    ) -> Result<Self> {
        Self::from_dirs_with_policy(dirs, None, ExternalLockfileMode::Off, lifecycle_recorder).await
    }

    pub async fn from_dirs_with_policy(
        dirs: &[PathBuf],
        lockfile: Option<ExternalToolsLockfile>,
        lockfile_mode: ExternalLockfileMode,
        lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    ) -> Result<Self> {
        Self::from_dirs_full(dirs, lockfile, lockfile_mode, lifecycle_recorder, None).await
    }

    /// Extended constructor accepting sandbox runtime wiring. Tool packages
    /// whose metadata declares `runtime.kind = "sandbox"` are routed through
    /// [`SandboxToolHandler`]; process packages follow the existing path.
    /// Missing wiring + sandbox metadata → hard error at load time.
    pub async fn from_dirs_with_sandbox(
        dirs: &[PathBuf],
        lockfile: Option<ExternalToolsLockfile>,
        lockfile_mode: ExternalLockfileMode,
        lifecycle_recorder: Option<ExternalLifecycleRecorder>,
        sandbox: SandboxRuntimeWiring,
    ) -> Result<Self> {
        Self::from_dirs_full(
            dirs,
            lockfile,
            lockfile_mode,
            lifecycle_recorder,
            Some(sandbox),
        )
        .await
    }

    async fn from_dirs_full(
        dirs: &[PathBuf],
        lockfile: Option<ExternalToolsLockfile>,
        lockfile_mode: ExternalLockfileMode,
        lifecycle_recorder: Option<ExternalLifecycleRecorder>,
        sandbox: Option<SandboxRuntimeWiring>,
    ) -> Result<Self> {
        let mut entries = HashMap::new();
        for dir in dirs {
            let (name, entry) =
                load_tool_dir(dir, lifecycle_recorder.as_ref(), sandbox.as_ref()).await?;
            if entries.contains_key(&name) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool '{}' loaded from {}",
                    name,
                    dir.display()
                )));
            }
            entries.insert(name, entry);
        }
        Ok(Self {
            entries,
            lockfile,
            lockfile_mode,
            lifecycle_recorder,
        })
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
        let Some(entry) = self.entries.get(name) else {
            return Ok(None);
        };

        let mut metadata = entry.metadata.clone();
        metadata.digest = Some(entry.digest.clone());

        match self.lockfile_mode {
            ExternalLockfileMode::Off => {
                return Ok(Some((metadata, entry.handler.clone())));
            }
            ExternalLockfileMode::Permissive | ExternalLockfileMode::Enforce => {}
        }

        let Some(lockfile) = self.lockfile.as_ref() else {
            self.emit_lockfile_artifact_event(
                name,
                &entry.artifact_ref,
                Some(entry.digest.clone()),
                "lockfile_missing",
                serde_json::json!({
                    "mode": format!("{:?}", self.lockfile_mode),
                    "computed_digest": entry.digest,
                }),
            );
            if self.lockfile_mode.should_enforce() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external lockfile missing while resolving '{name}'"
                )));
            }
            return Ok(Some((metadata, entry.handler.clone())));
        };

        let Some(lock_entry) = lockfile.by_name(name) else {
            self.emit_lockfile_artifact_event(
                name,
                &entry.artifact_ref,
                Some(entry.digest.clone()),
                "lockfile_entry_missing",
                serde_json::json!({
                    "computed_digest": entry.digest,
                }),
            );
            if self.lockfile_mode.should_enforce() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external lockfile missing entry for tool '{name}'"
                )));
            }
            return Ok(Some((metadata, entry.handler.clone())));
        };

        if lock_entry.digest != entry.digest {
            self.emit_lockfile_artifact_event(
                name,
                &entry.artifact_ref,
                Some(entry.digest.clone()),
                "digest_mismatch",
                serde_json::json!({
                    "expected_digest": lock_entry.digest,
                    "computed_digest": entry.digest,
                }),
            );
            if self.lockfile_mode.should_enforce() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool digest mismatch for '{name}': expected {}, got {}",
                    lock_entry.digest, entry.digest
                )));
            }
        }

        Ok(Some((metadata, entry.handler.clone())))
    }
}

impl DevModeResolver {
    fn emit_lockfile_artifact_event(
        &self,
        tool_name: &ToolName,
        artifact_ref: &str,
        digest: Option<String>,
        verification_result: &str,
        details: serde_json::Value,
    ) {
        if let Some(recorder) = &self.lifecycle_recorder {
            recorder(ExternalLifecycleEvent::Artifact {
                tool_name: tool_name.to_string(),
                artifact_ref: artifact_ref.to_string(),
                digest,
                signer: None,
                verification_result: verification_result.to_string(),
                pull_latency_ms: None,
                details,
            });
        }
    }
}

async fn load_tool_dir(
    dir: &Path,
    lifecycle_recorder: Option<&ExternalLifecycleRecorder>,
    sandbox: Option<&SandboxRuntimeWiring>,
) -> Result<(ToolName, DevToolEntry)> {
    let metadata_path = dir.join("tool-metadata.json");

    if !metadata_path.exists() {
        if let Some(recorder) = lifecycle_recorder {
            recorder(ExternalLifecycleEvent::Artifact {
                tool_name: "unknown".to_string(),
                artifact_ref: metadata_path.display().to_string(),
                digest: None,
                signer: None,
                verification_result: "metadata_missing".to_string(),
                pull_latency_ms: None,
                details: serde_json::json!({ "dir": dir.display().to_string() }),
            });
        }
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool metadata not found at {}",
            metadata_path.display()
        )));
    }

    let meta = read_runtime_external_metadata(dir)?;
    let tool_name = ToolName::parse(&meta.name)?;
    let runtime_kind = meta.runtime.as_ref().map(ToolRuntime::kind);

    if matches!(meta.invocation_mode, InvocationMode::Session)
        && !matches!(
            runtime_kind,
            Some(crate::external_tools::runtime::ToolRuntimeKind::Sandbox)
        )
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "tool '{tool_name}' sets invocation_mode=session but is not sandbox runtime; session mode is sandbox-only"
        )));
    }

    // Dispatch by runtime kind (tool_sandbox.md Workstream B step 6).
    // - None / Some(Process) → existing subprocess + stdio path.
    // - Some(Sandbox)        → SandboxToolHandler (requires sandbox wiring).
    match runtime_kind {
        Some(crate::external_tools::runtime::ToolRuntimeKind::Sandbox) => {
            load_sandbox_tool_dir(dir, meta, tool_name, sandbox, lifecycle_recorder).await
        }
        _ => load_process_tool_dir(dir, meta, tool_name, lifecycle_recorder).await,
    }
}

async fn load_process_tool_dir(
    dir: &Path,
    meta: ExternalToolMetadata,
    tool_name: ToolName,
    lifecycle_recorder: Option<&ExternalLifecycleRecorder>,
) -> Result<(ToolName, DevToolEntry)> {
    let bin_path = dir.join("tool-server");

    if !bin_path.exists() {
        if let Some(recorder) = lifecycle_recorder {
            recorder(ExternalLifecycleEvent::Artifact {
                tool_name: meta.name.clone(),
                artifact_ref: bin_path.display().to_string(),
                digest: None,
                signer: None,
                verification_result: "binary_missing".to_string(),
                pull_latency_ms: None,
                details: serde_json::json!({ "dir": dir.display().to_string() }),
            });
        }
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool binary not found at {}",
            bin_path.display()
        )));
    }

    let digest = compute_tool_digest(dir)?;
    let mut metadata = build_tool_metadata(dir, &meta, &tool_name)?;
    metadata.digest = Some(digest.clone());

    if let Some(recorder) = lifecycle_recorder {
        recorder(ExternalLifecycleEvent::Artifact {
            tool_name: tool_name.to_string(),
            artifact_ref: bin_path.display().to_string(),
            digest: Some(digest.clone()),
            signer: None,
            verification_result: "dev_mode_local_present".to_string(),
            pull_latency_ms: Some(0),
            details: serde_json::json!({ "dir": dir.display().to_string() }),
        });
    }

    let invoker = Arc::new(StdioSubprocessInvoker::new(bin_path.clone()));
    let describe =
        describe_with_cache(invoker.as_ref(), &tool_name, &bin_path, lifecycle_recorder).await?;
    validate_describe_contract(&meta, &tool_name, &describe)?;

    let mut handler_builder =
        ProcessToolHandler::new(metadata.clone(), invoker, DEFAULT_INVOKE_TIMEOUT)
            .with_capabilities(meta.capabilities.clone());
    if let Some(recorder) = lifecycle_recorder {
        handler_builder = handler_builder.with_lifecycle_recorder(recorder.clone());
    }
    let handler: Arc<dyn ToolHandler> = Arc::new(handler_builder);

    let artifact_ref = bin_path.display().to_string();
    Ok((
        tool_name,
        DevToolEntry {
            metadata,
            handler,
            digest,
            artifact_ref,
        },
    ))
}

async fn load_sandbox_tool_dir(
    dir: &Path,
    meta: ExternalToolMetadata,
    tool_name: ToolName,
    sandbox: Option<&SandboxRuntimeWiring>,
    lifecycle_recorder: Option<&ExternalLifecycleRecorder>,
) -> Result<(ToolName, DevToolEntry)> {
    let wiring = sandbox.ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "tool '{tool_name}' declares sandbox runtime but no sandbox wiring was provided to the resolver (see DevModeResolver::from_dirs_with_sandbox)"
        ))
    })?;

    // Sandbox-kind tools don't ship a tool-server binary — the adapter lives
    // inside the image. `compute_tool_digest` covers the sandbox branch
    // (`baml-ext-tool-sandbox-v1\0` magic + canonical metadata bytes), so the
    // lockfile sees schema/runtime changes instead of a name-only stub.
    let digest = compute_tool_digest(dir)?;
    let mut metadata = build_tool_metadata(dir, &meta, &tool_name)?;
    metadata.digest = Some(digest.clone());

    if let Some(recorder) = lifecycle_recorder {
        recorder(ExternalLifecycleEvent::Artifact {
            tool_name: tool_name.to_string(),
            artifact_ref: dir.display().to_string(),
            digest: Some(digest.clone()),
            signer: None,
            verification_result: "sandbox_runtime_declared".to_string(),
            pull_latency_ms: None,
            details: serde_json::json!({
                "dir": dir.display().to_string(),
                "runtime_kind": "sandbox",
            }),
        });
    }

    // describe() is deferred to first invoke for sandbox tools — issuing a
    // describe here would require materializing a sandbox per tool at
    // resolve time, which conflicts with §9.4 lazy first-use. Contract
    // validation still happens once per (agent_instance, context) at the
    // first invoke.
    let spec_builder = (wiring.spec_factory)(&tool_name, &meta)?;

    let handler: Arc<dyn ToolHandler> = match meta.invocation_mode {
        InvocationMode::SingleShot => {
            let mut handler_builder = SandboxToolHandler::new(
                metadata.clone(),
                wiring.provider.clone(),
                wiring.cache.clone(),
                spec_builder,
                DEFAULT_INVOKE_TIMEOUT,
            )
            .with_capabilities(meta.capabilities.clone());
            if let Some(recorder) = lifecycle_recorder {
                handler_builder = handler_builder.with_lifecycle_recorder(recorder.clone());
            }
            Arc::new(handler_builder)
        }
        InvocationMode::Session => {
            // TODO(phase-4 sandbox-streaming §7.2/§9.4): wire per-tool
            // configuration from metadata into the pool/invoker instead of
            // using the type defaults below. Specifically:
            //   - `meta.session_policy` (Strict / MultiSend) — should drive
            //     `SandboxSessionInvokerConfig` once a corresponding field
            //     exists; today every tool gets the same FSM enforcement.
            //   - `meta.reuse_after_session` — must override
            //     `SandboxSessionInvokerConfig::reuse_after_session`
            //     (default `false`); right now opt-in reuse is dropped on
            //     the floor so every finish destroys the sandbox.
            //   - `SessionPoolConfig::default_pool_max` /
            //     `pool_checkout_timeout` — should pull from a future
            //     `meta.pool_*` block once Phase 4 lands; today every tool
            //     shares the global default cap.
            let pool = Arc::new(SessionPool::new(
                wiring.cache.runner_id().to_string(),
                wiring.provider.clone(),
                spec_builder,
                SessionPoolConfig::default(),
            ));
            let invoker_config = SandboxSessionInvokerConfig::default();
            let invoker_factory = {
                let pool = pool.clone();
                Arc::new(move |ctx: &ToolSessionContext| {
                    Arc::new(SandboxSessionInvoker::new(
                        pool.clone(),
                        ctx.agent_id.clone(),
                        ctx.context_id.clone(),
                        invoker_config.clone(),
                    )) as Arc<dyn super::SessionToolInvoker>
                })
            };

            Arc::new(
                ExternalSessionToolHandler::new_with_factory(
                    metadata.clone(),
                    invoker_factory,
                    DEFAULT_INVOKE_TIMEOUT,
                )
                .with_capabilities(meta.capabilities.clone())
                .with_secret_scope(meta.secret_scope),
            )
        }
    };

    let artifact_ref = dir.display().to_string();
    Ok((
        tool_name,
        DevToolEntry {
            metadata,
            handler,
            digest,
            artifact_ref,
        },
    ))
}

async fn describe_with_cache(
    invoker: &StdioSubprocessInvoker,
    tool_name: &ToolName,
    binary_path: &Path,
    lifecycle_recorder: Option<&ExternalLifecycleRecorder>,
) -> Result<ToolDescribe> {
    let cache_key = DescribeCacheKey {
        tool_name: tool_name.to_string(),
        identity: dev_identity(binary_path)?,
    };

    let cache = DESCRIBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.get(&cache_key)
    {
        if let Some(recorder) = lifecycle_recorder {
            recorder(ExternalLifecycleEvent::Describe {
                tool_name: tool_name.to_string(),
                identity: Some(cache_key.identity.clone()),
                protocol_version: Some(cached.protocol_version.clone()),
                latency_ms: 0,
                result: "cache_hit".to_string(),
                details: serde_json::json!({ "supported_methods": cached.supported_methods.clone() }),
            });
        }
        return Ok(cached.clone());
    }

    let started = std::time::Instant::now();
    let describe = match invoker.describe(tool_name, DEFAULT_DESCRIBE_TIMEOUT).await {
        Ok(d) => d,
        Err(err) => {
            if let Some(recorder) = lifecycle_recorder {
                recorder(ExternalLifecycleEvent::Describe {
                    tool_name: tool_name.to_string(),
                    identity: Some(cache_key.identity.clone()),
                    protocol_version: None,
                    latency_ms: started.elapsed().as_millis() as u64,
                    result: "failed".to_string(),
                    details: serde_json::json!({ "error": err.to_string() }),
                });
            }
            return Err(err);
        }
    };

    if let Some(recorder) = lifecycle_recorder {
        recorder(ExternalLifecycleEvent::Describe {
            tool_name: tool_name.to_string(),
            identity: Some(cache_key.identity.clone()),
            protocol_version: Some(describe.protocol_version.clone()),
            latency_ms: started.elapsed().as_millis() as u64,
            result: "ok".to_string(),
            details: serde_json::json!({
                "supported_methods": describe.supported_methods.clone(),
                "schema_digest": describe.schema_digest.clone(),
            }),
        });
    }

    if let Ok(mut guard) = cache.lock() {
        guard.insert(cache_key, describe.clone());
    }

    Ok(describe)
}

fn dev_identity(binary_path: &Path) -> Result<String> {
    let canonical =
        std::fs::canonicalize(binary_path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to canonicalize external tool binary path {}",
                binary_path.display()
            ),
            source: Box::new(e),
        })?;
    let stat =
        std::fs::metadata(&canonical).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to stat external tool binary at {}",
                canonical.display()
            ),
            source: Box::new(e),
        })?;

    let modified_ns = stat
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    Ok(format!(
        "{}:{}:{}",
        canonical.display(),
        stat.len(),
        modified_ns
    ))
}

fn validate_describe_contract(
    meta: &ExternalToolMetadata,
    tool_name: &ToolName,
    describe: &ToolDescribe,
) -> Result<()> {
    if describe.tool_name != meta.name {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' describe mismatch: metadata name '{}' != describe name '{}'",
            tool_name, meta.name, describe.tool_name
        )));
    }

    if describe.protocol_version != PROTOCOL_VERSION {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' describe mismatch: metadata protocol '{}' != describe protocol '{}'",
            tool_name, PROTOCOL_VERSION, describe.protocol_version
        )));
    }

    match meta.invocation_mode {
        InvocationMode::SingleShot => {
            if !describe
                .supported_methods
                .iter()
                .any(|method| method == METHOD_INVOKE)
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool '{}' describe mismatch: supported_methods must include '{}'",
                    tool_name, METHOD_INVOKE
                )));
            }
        }
        InvocationMode::Session => {
            for required in baml_sandbox_protocol::SUPPORTED_METHODS_SESSION {
                if !describe
                    .supported_methods
                    .iter()
                    .any(|method| method == required)
                {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "external tool '{}' describe mismatch: supported_methods must include '{}' for invocation_mode=session",
                        tool_name, required
                    )));
                }
            }
        }
    }

    if let Some(describe_schema_digest) = describe.schema_digest.as_ref() {
        let expected = metadata_schema_digest(meta);
        if describe_schema_digest != &expected {
            return Err(BamlRtError::InvalidArgument(format!(
                "external tool '{}' describe mismatch: metadata schema digest '{}' != describe schema digest '{}'",
                tool_name, expected, describe_schema_digest
            )));
        }
    }

    if let Some(describe_capabilities) = describe.capabilities.as_ref()
        && describe_capabilities != &meta.capabilities
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' describe mismatch: metadata capabilities contradict describe capabilities",
            tool_name
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use serde_json::json;
    use uuid::Uuid;

    use super::{DevModeResolver, ExternalLifecycleEvent, ExternalLifecycleRecorder};
    use crate::{
        ExternalToolResolver, ToolName,
        external_tools::{
            ToolDescribe,
            metadata::{ExternalToolMetadata, InvocationMode, metadata_schema_digest},
        },
        tools::SessionPolicy,
    };

    #[tokio::test]
    async fn dev_mode_resolver_accepts_matching_describe_and_caches() {
        let base = unique_temp_dir("external-tool-cache-ok");
        let tool_dir = base.join("tool");
        fs::create_dir_all(&tool_dir).unwrap();

        let tool_name = "support/cache_ok";
        let schemas = json!({
            "input": {"type": "object", "properties": {"x": {"type": "string"}}},
            "output": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
        });
        let metadata = json!({
            "tool_abi_version": "1",
            "name": tool_name,
            "description": "cache test",
            "bundle": "support",
            "local_name": "cache_ok",
            "access_level": "read",
            "tags": [],
            "invocation_mode": "single_shot",
            "schemas": schemas,
            "secrets": [],
            "capabilities": {"http": {"hosts": ["api.example.com"]}}
        });

        fs::write(
            tool_dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        let schema_digest = metadata_schema_digest(&serde_json::from_value(metadata).unwrap());
        let counter_path = tool_dir.join("describe-count");
        let response = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{tool_name}\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\"],\"schema_digest\":\"{schema_digest}\"}}}}"
        );
        write_tool_server(
            &tool_dir.join("tool-server"),
            &format!(
                "#!/bin/sh\n\
count=0\n\
if [ -f '{counter}' ]; then read count < '{counter}'; fi\n\
count=$((count+1))\n\
printf '%s' \"$count\" > '{counter}'\n\
while IFS= read -r _; do :; done\n\
printf '%s\\n' '{response}'\n",
                counter = counter_path.display(),
                response = response,
            ),
        );

        let _resolver_1 = DevModeResolver::from_dirs(std::slice::from_ref(&tool_dir))
            .await
            .expect("first load should succeed");
        let _resolver_2 = DevModeResolver::from_dirs(std::slice::from_ref(&tool_dir))
            .await
            .expect("second load should reuse cache");

        let count: u64 = fs::read_to_string(counter_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            count, 1,
            "describe should be cached for stable dev identity"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn dev_mode_resolver_rejects_describe_name_mismatch() {
        let base = unique_temp_dir("external-tool-mismatch");
        let tool_dir = base.join("tool");
        fs::create_dir_all(&tool_dir).unwrap();

        let metadata = json!({
            "tool_abi_version": "1",
            "name": "support/name_ok",
            "description": "mismatch test",
            "bundle": "support",
            "local_name": "name_ok",
            "access_level": "read",
            "tags": [],
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });

        fs::write(
            tool_dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        write_tool_server(
            &tool_dir.join("tool-server"),
            "#!/bin/sh\nwhile IFS= read -r _; do :; done\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocol_version\":\"1\",\"tool_name\":\"support/not_the_same\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\"]}}'\n",
        );

        let err = match DevModeResolver::from_dirs(std::slice::from_ref(&tool_dir)).await {
            Ok(_) => panic!("mismatch must fail closed"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("describe mismatch") && msg.contains("metadata name"),
            "unexpected error: {msg}"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn dev_mode_resolver_maps_multi_send_session_policy_from_metadata() {
        let base = unique_temp_dir("external-tool-session-policy");
        let tool_dir = base.join("tool");
        fs::create_dir_all(&tool_dir).unwrap();

        let tool_name = "support/multisend_external";
        let metadata = json!({
            "tool_abi_version": "1",
            "name": tool_name,
            "description": "session policy mapping test",
            "bundle": "support",
            "local_name": "multisend_external",
            "access_level": "read",
            "tags": [],
            "invocation_mode": "single_shot",
            "session_policy": "multi_send",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });

        fs::write(
            tool_dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        write_tool_server(
            &tool_dir.join("tool-server"),
            &format!(
                "#!/bin/sh\nwhile IFS= read -r _; do :; done\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{tool_name}\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\"]}}}}'\n"
            ),
        );

        let resolver = DevModeResolver::from_dirs(std::slice::from_ref(&tool_dir))
            .await
            .expect("resolver load should succeed");

        let parsed_name = ToolName::parse(tool_name).unwrap();
        let (resolved_meta, _handler) = resolver
            .resolve(&parsed_name)
            .expect("resolver query ok")
            .expect("tool must resolve");

        assert_eq!(resolved_meta.session_policy, SessionPolicy::MultiSend);

        let _ = fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn dev_mode_resolver_emits_describe_and_artifact_lifecycle_events() {
        let base = unique_temp_dir("external-tool-lifecycle-events");
        let tool_dir = base.join("tool");
        fs::create_dir_all(&tool_dir).unwrap();

        let tool_name = "support/lifecycle_events";
        let metadata = json!({
            "tool_abi_version": "1",
            "name": tool_name,
            "description": "lifecycle test",
            "bundle": "support",
            "local_name": "lifecycle_events",
            "access_level": "read",
            "tags": [],
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });

        fs::write(
            tool_dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        write_tool_server(
            &tool_dir.join("tool-server"),
            &format!(
                "#!/bin/sh\nwhile IFS= read -r _; do :; done\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{tool_name}\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\"]}}}}'\n"
            ),
        );

        let captured = Arc::new(Mutex::new(Vec::<ExternalLifecycleEvent>::new()));
        let recorder: ExternalLifecycleRecorder = {
            let captured = captured.clone();
            Arc::new(move |event| {
                captured.lock().unwrap().push(event);
            })
        };

        let _resolver = DevModeResolver::from_dirs_with_lifecycle(
            std::slice::from_ref(&tool_dir),
            Some(recorder),
        )
        .await
        .expect("resolver load should succeed");

        let events = captured.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ExternalLifecycleEvent::Artifact { .. })),
            "expected artifact lifecycle event"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ExternalLifecycleEvent::Describe { .. })),
            "expected describe lifecycle event"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn validate_describe_requires_session_method_set_for_session_mode() {
        let meta: ExternalToolMetadata = serde_json::from_value(json!({
            "tool_abi_version": "1",
            "name": "support/session_contract",
            "description": "session contract",
            "bundle": "support",
            "local_name": "session_contract",
            "access_level": "read",
            "invocation_mode": "session",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        }))
        .expect("metadata parse");
        assert!(matches!(meta.invocation_mode, InvocationMode::Session));

        let describe = ToolDescribe {
            protocol_version: "1".to_string(),
            tool_name: "support/session_contract".to_string(),
            supported_methods: vec![
                "tool/describe".to_string(),
                "tool/schema".to_string(),
                "tool/session_open".to_string(),
                "tool/session_send".to_string(),
                "tool/session_read".to_string(),
                // Missing finish + abort on purpose.
            ],
            max_payload_bytes: None,
            schema_digest: None,
            capabilities: None,
        };

        let tool_name = ToolName::parse("support/session_contract").unwrap();
        let err = super::validate_describe_contract(&meta, &tool_name, &describe)
            .expect_err("missing methods should fail");
        let msg = err.to_string();
        assert!(msg.contains("invocation_mode=session"), "unexpected: {msg}");
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()))
    }

    fn write_tool_server(path: &Path, script: &str) {
        fs::write(path, script.as_bytes()).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}
