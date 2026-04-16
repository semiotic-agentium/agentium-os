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
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    ExternalLifecycleEvent, ExternalLifecycleRecorder,
    handler::ProcessToolHandler,
    invoker::{ExternalInvoker, ToolDescribe},
    policy::{DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT},
    protocol::PROTOCOL_VERSION,
    stdio::StdioSubprocessInvoker,
};
use crate::{
    ExternalToolResolver, ToolName,
    tools::{
        BundleName, SecretRequest, SessionPolicy, ToolAccess, ToolBackend, ToolFunctionMetadata,
        ToolHandler, ToolOrigin, ToolTypeSpec,
    },
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
    #[serde(default)]
    session_policy: RawSessionPolicy,
    schemas: RawSchemas,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    capabilities: Value,
    #[serde(default)]
    config_bundle: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawSessionPolicy {
    #[default]
    Strict,
    MultiSend,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSchemas {
    input: Value,
    output: Value,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct DescribeCacheKey {
    tool_name: String,
    identity: String,
}

static DESCRIBE_CACHE: OnceLock<Mutex<HashMap<DescribeCacheKey, ToolDescribe>>> = OnceLock::new();

/// Local-filesystem resolver built from a set of tool package directories.
pub struct DevModeResolver {
    entries: HashMap<ToolName, (ToolFunctionMetadata, Arc<dyn ToolHandler>)>,
}

impl DevModeResolver {
    /// Load all tool packages from the supplied directories. Each directory
    /// must contain `tool-metadata.json` and a `tool-server` executable.
    pub async fn from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        Self::from_dirs_with_lifecycle(dirs, None).await
    }

    /// Same as [`Self::from_dirs`] but emits lifecycle callbacks for external-tool
    /// describe/artifact operations when a recorder is provided.
    pub async fn from_dirs_with_lifecycle(
        dirs: &[PathBuf],
        lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    ) -> Result<Self> {
        let mut entries = HashMap::new();
        for dir in dirs {
            let (name, metadata, handler) = load_tool_dir(dir, lifecycle_recorder.as_ref()).await?;
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

async fn load_tool_dir(
    dir: &Path,
    lifecycle_recorder: Option<&ExternalLifecycleRecorder>,
) -> Result<(ToolName, ToolFunctionMetadata, Arc<dyn ToolHandler>)> {
    let metadata_path = dir.join("tool-metadata.json");
    let bin_path = dir.join("tool-server");

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
        if let Some(recorder) = lifecycle_recorder {
            recorder(ExternalLifecycleEvent::Artifact {
                tool_name: raw.name.clone(),
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

    let tool_name = ToolName::parse(&raw.name)?;
    let metadata = build_metadata(&raw, &tool_name)?;

    if let Some(recorder) = lifecycle_recorder {
        recorder(ExternalLifecycleEvent::Artifact {
            tool_name: tool_name.to_string(),
            artifact_ref: bin_path.display().to_string(),
            digest: None,
            signer: None,
            verification_result: "dev_mode_local_present".to_string(),
            pull_latency_ms: Some(0),
            details: serde_json::json!({ "dir": dir.display().to_string() }),
        });
    }

    let invoker = Arc::new(StdioSubprocessInvoker::new(bin_path.clone()));
    let describe =
        describe_with_cache(invoker.as_ref(), &tool_name, &bin_path, lifecycle_recorder).await?;
    validate_describe_contract(&raw, &tool_name, &describe)?;

    let mut handler_builder =
        ProcessToolHandler::new(metadata.clone(), invoker, DEFAULT_INVOKE_TIMEOUT)
            .with_capabilities(raw.capabilities.clone());
    if let Some(recorder) = lifecycle_recorder {
        handler_builder = handler_builder.with_lifecycle_recorder(recorder.clone());
    }
    let handler: Arc<dyn ToolHandler> = Arc::new(handler_builder);

    Ok((tool_name, metadata, handler))
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
                "schema_hash": describe.schema_hash.clone(),
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
    raw: &RawToolMetadata,
    tool_name: &ToolName,
    describe: &ToolDescribe,
) -> Result<()> {
    if describe.tool_name != raw.name {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' describe mismatch: metadata name '{}' != describe name '{}'",
            tool_name, raw.name, describe.tool_name
        )));
    }

    if describe.protocol_version != PROTOCOL_VERSION {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' describe mismatch: metadata protocol '{}' != describe protocol '{}'",
            tool_name, PROTOCOL_VERSION, describe.protocol_version
        )));
    }

    if !describe
        .supported_methods
        .iter()
        .any(|method| method == "tool/invoke")
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' describe mismatch: supported_methods must include 'tool/invoke'",
            tool_name
        )));
    }

    if let Some(describe_schema_hash) = describe.schema_hash.as_ref() {
        let expected = metadata_schema_hash(raw);
        if describe_schema_hash != &expected {
            return Err(BamlRtError::InvalidArgument(format!(
                "external tool '{}' describe mismatch: metadata schema hash '{}' != describe schema hash '{}'",
                tool_name, expected, describe_schema_hash
            )));
        }
    }

    if let Some(describe_capabilities) = describe.capabilities.as_ref()
        && describe_capabilities != &raw.capabilities
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' describe mismatch: metadata capabilities contradict describe capabilities",
            tool_name
        )));
    }

    Ok(())
}

fn metadata_schema_hash(raw: &RawToolMetadata) -> String {
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
    use crate::{ExternalToolResolver, ToolName, tools::SessionPolicy};

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

        let schema_hash = super::metadata_schema_hash(&serde_json::from_value(metadata).unwrap());
        let counter_path = tool_dir.join("describe-count");
        let response = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{tool_name}\",\"supported_methods\":[\"tool/invoke\"],\"schema_hash\":\"{schema_hash}\"}}}}"
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
            "#!/bin/sh\nwhile IFS= read -r _; do :; done\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocol_version\":\"1\",\"tool_name\":\"support/not_the_same\",\"supported_methods\":[\"tool/invoke\"]}}'\n",
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
                "#!/bin/sh\nwhile IFS= read -r _; do :; done\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{tool_name}\",\"supported_methods\":[\"tool/invoke\"]}}}}'\n"
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
                "#!/bin/sh\nwhile IFS= read -r _; do :; done\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{tool_name}\",\"supported_methods\":[\"tool/invoke\"]}}}}'\n"
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
