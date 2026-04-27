//! External tool metadata — the contract for `tool-metadata.json`.
//!
//! [`ExternalToolMetadata`] is the public typed model for everything a scaffolder
//! writes and the runtime reads. Keeping one struct means the CLI scaffolder
//! (`cargo-agent-platform`), the build-time catalog
//! ([`super::metadata_catalog::ExternalMetadataCatalog`]), and the runtime
//! resolver ([`super::resolver::DevModeResolver`]) cannot drift from each
//! other — field renames become compile errors, not schema mismatches.
//!
//! Downstream machinery (secret requests, session policies, digest helpers)
//! still lives here so there is a single place where the metadata file's
//! semantics are defined.

use std::{fs, path::Path};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{protocol::PROTOCOL_VERSION, runtime::ToolRuntime};
use crate::{
    ToolName,
    tools::{
        BundleName, SecretRequest, SessionPolicy, ToolAccess, ToolBackend, ToolFunctionMetadata,
        ToolOrigin, ToolTypeSpec,
    },
};

/// Typed representation of `tool-metadata.json`.
///
/// The struct *is* the schema: renaming a field here is a compile-error for
/// every consumer (CLI scaffolder writer, runtime reader). Optional fields use
/// `Option<_>` / `#[serde(default)]` so hand-written metadata can omit them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolMetadata {
    /// Protocol ABI version. V1 only accepts `"1"`.
    pub tool_abi_version: String,
    /// Fully qualified tool name (`bundle/local_name`).
    pub name: String,
    /// Human-readable description surfaced in discovery listings.
    pub description: String,
    /// Bundle namespace (free-form; validated via [`BundleName`]).
    pub bundle: String,
    /// Local tool name within the bundle.
    pub local_name: String,
    /// Permission level the tool needs; `read` / `write` / `delete`.
    pub access_level: ToolAccess,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Invocation semantics — V1 supports single-shot only.
    pub invocation_mode: InvocationMode,
    /// FSM scheduling policy. Defaults to `Strict`.
    #[serde(default)]
    pub session_policy: ExternalSessionPolicy,
    /// Input and output JSON Schemas.
    pub schemas: MetadataSchemas,
    /// Secret names the runtime must resolve for this tool.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Capability declaration (HTTP hosts, FS access, etc.). Free-form JSON
    /// until Phase 2-full formalises it into a typed struct.
    #[serde(default)]
    pub capabilities: Value,
    /// Bundle key for config store lookup (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_bundle: Option<String>,
    /// Optional execution-runtime declaration. Missing => process mode with
    /// the wrapper default (§4.2 of `tool_sandbox.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<ToolRuntime>,
    /// Runtime identity digest for sandbox runtimes (`sha256:<hex>`).
    ///
    /// Required when `runtime.kind == "sandbox"`; omitted for process tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_digest: Option<String>,
}

impl ExternalToolMetadata {
    /// Construct a single-shot external tool with default optional fields.
    ///
    /// The scaffolder uses this to avoid hand-rolling JSON; the runtime never
    /// calls it (metadata comes in via `serde::Deserialize` on disk bytes).
    pub fn new(
        name: impl Into<String>,
        bundle: impl Into<String>,
        local_name: impl Into<String>,
        access_level: ToolAccess,
        description: impl Into<String>,
        input_schema: Value,
        output_schema: Value,
    ) -> Self {
        Self {
            tool_abi_version: PROTOCOL_VERSION.to_string(),
            name: name.into(),
            description: description.into(),
            bundle: bundle.into(),
            local_name: local_name.into(),
            access_level,
            tags: Vec::new(),
            invocation_mode: InvocationMode::SingleShot,
            session_policy: ExternalSessionPolicy::default(),
            schemas: MetadataSchemas {
                input: input_schema,
                output: output_schema,
            },
            secrets: Vec::new(),
            capabilities: Value::Object(Default::default()),
            config_bundle: None,
            runtime: None,
            runtime_digest: None,
        }
    }

    /// Set discovery tags on this metadata (builder-style).
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Serialize to pretty JSON with trailing newline, the shape the CLI
    /// writes into `tool-metadata.json`.
    pub fn to_pretty_json(&self) -> Result<String> {
        let mut out = serde_json::to_string_pretty(self).map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "failed to serialize external tool metadata".to_string(),
                source: Box::new(e),
            }
        })?;
        out.push('\n');
        Ok(out)
    }
}

/// Invocation semantics declared by the tool. V1 only supports single-shot
/// (stateless spawn-per-invoke); streaming/keep-alive lands in a future phase
/// and will add a new variant — the compiler enforces we handle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationMode {
    SingleShot,
}

/// Session policy encoded in the external metadata file. Separate from the
/// runtime's [`SessionPolicy`] only because serde rename semantics differ;
/// round-trips losslessly via [`ExternalSessionPolicy::to_session_policy`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionPolicy {
    #[default]
    Strict,
    MultiSend,
}

impl ExternalSessionPolicy {
    pub fn to_session_policy(self) -> SessionPolicy {
        match self {
            Self::Strict => SessionPolicy::Strict,
            Self::MultiSend => SessionPolicy::MultiSend,
        }
    }
}

/// Input/output JSON Schemas carried in the metadata file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataSchemas {
    pub input: Value,
    pub output: Value,
}

/// Read + validate `<dir>/tool-metadata.json` into the typed model.
///
/// Typed deserialization rejects malformed enums (e.g. an unknown
/// `access_level`) before the runtime sees them, so there's no extra manual
/// string-matching for access/session/invocation fields.
pub fn read_external_metadata(dir: &Path) -> Result<ExternalToolMetadata> {
    let metadata_path = dir.join("tool-metadata.json");
    let raw = std::fs::read_to_string(&metadata_path).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to read {}", metadata_path.display()),
            source: Box::new(e),
        }
    })?;
    let parsed: ExternalToolMetadata =
        serde_json::from_str(&raw).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to parse {}", metadata_path.display()),
            source: Box::new(e),
        })?;

    if parsed.tool_abi_version != PROTOCOL_VERSION {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' declares unsupported ABI version '{}' (expected '{}')",
            parsed.name, parsed.tool_abi_version, PROTOCOL_VERSION
        )));
    }

    Ok(parsed)
}

/// Deprecated alias kept for the old call sites still in migration.
/// Remove once every caller points at [`read_external_metadata`].
#[deprecated(note = "use read_external_metadata")]
#[allow(dead_code)]
pub(crate) fn read_raw_metadata(dir: &Path) -> Result<ExternalToolMetadata> {
    read_external_metadata(dir)
}

/// Project parsed metadata into the runtime [`ToolFunctionMetadata`] shape.
pub(crate) fn build_tool_metadata(
    meta: &ExternalToolMetadata,
    tool_name: &ToolName,
) -> Result<ToolFunctionMetadata> {
    let class_name = ToolFunctionMetadata::derive_class_name(tool_name.bundle(), tool_name.local());

    let config_bundle = match &meta.config_bundle {
        Some(s) => Some(BundleName::new(s)?),
        None => None,
    };

    let secret_requests: Vec<SecretRequest> = meta
        .secrets
        .iter()
        .map(|s| {
            SecretRequest::api_key(
                s.clone(),
                format!("Required by external tool {}", meta.name),
                s.clone(),
            )
        })
        .collect();

    Ok(ToolFunctionMetadata {
        name: tool_name.clone(),
        class_name: class_name.clone(),
        description: meta.description.clone(),
        // External tools have no "open input" concept in V1 — single-shot invoke.
        open_input_schema: serde_json::json!({}),
        input_schema: meta.schemas.input.clone(),
        output_schema: meta.schemas.output.clone(),
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
        access: Some(meta.access_level),
        tags: meta.tags.clone(),
        secret_requests,
        config: None,
        config_bundle,
        origin: ToolOrigin::Host,
        backend: ToolBackend::External,
        digest: None,
        projection_semantics: None,
        session_policy: meta.session_policy.to_session_policy(),
        event_sources: Vec::new(),
    })
}

/// RFC 8785 (JCS) canonical SHA-256 of the input+output schemas.
///
/// Both runner and tool author must compute this identically for
/// describe-mismatch detection to work.
pub(crate) fn metadata_schema_digest(meta: &ExternalToolMetadata) -> String {
    let payload = serde_json::json!({
        "input": &meta.schemas.input,
        "output": &meta.schemas.output,
    });
    let canonical = serde_jcs::to_vec(&payload)
        .expect("serializing canonical tool schema payload should not fail");
    let mut hasher = Sha256::new();
    hasher.update(&canonical);
    format!("sha256:{:x}", hasher.finalize())
}

/// Compute deterministic digest for a local external tool package directory.
///
/// Process runtime input (in order):
/// - magic/version marker: `baml-ext-tool-v1\0`
/// - tool binary bytes prefixed by u64 little-endian length
/// - canonicalized metadata bytes prefixed by u64 little-endian length
/// - filesystem mode bits (`stat().mode() & 0o7777`) as u32 little-endian
///
/// Sandbox runtime input (in order):
/// - magic/version marker: `baml-ext-tool-sandbox-v1\0`
/// - canonicalized metadata bytes prefixed by u64 little-endian length
///
/// Sandbox tools intentionally do not require a local `tool-server` binary:
/// runtime identity is declared by sandbox metadata (`runtime.image`,
/// `runtime_digest`) rather than host executable bytes.
pub fn compute_tool_digest(dir: &Path) -> Result<String> {
    let metadata_path = dir.join("tool-metadata.json");
    let metadata_raw =
        fs::read_to_string(&metadata_path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to read external tool metadata {}",
                metadata_path.display()
            ),
            source: Box::new(e),
        })?;
    let metadata_json: Value = serde_json::from_str(&metadata_raw).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to parse external tool metadata {}",
                metadata_path.display()
            ),
            source: Box::new(e),
        }
    })?;
    let metadata: ExternalToolMetadata = serde_json::from_str(&metadata_raw).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to decode typed external tool metadata {}",
                metadata_path.display()
            ),
            source: Box::new(e),
        }
    })?;
    let canonical_metadata =
        serde_json::to_string(&sort_json_keys(&metadata_json)).map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "failed to canonicalize external tool metadata JSON".to_string(),
                source: Box::new(e),
            }
        })?;

    if matches!(metadata.runtime, Some(ToolRuntime::Sandbox(_))) {
        let mut hasher = Sha256::new();
        hasher.update(b"baml-ext-tool-sandbox-v1\0");
        hasher.update((canonical_metadata.len() as u64).to_le_bytes());
        hasher.update(canonical_metadata.as_bytes());
        return Ok(format!("sha256:{:x}", hasher.finalize()));
    }

    let bin_path = dir.join("tool-server");
    let bin_bytes = fs::read(&bin_path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
        message: format!("failed to read external tool binary {}", bin_path.display()),
        source: Box::new(e),
    })?;
    let mode_bits = file_mode_bits(&bin_path)?;

    let mut hasher = Sha256::new();
    hasher.update(b"baml-ext-tool-v1\0");
    hasher.update((bin_bytes.len() as u64).to_le_bytes());
    hasher.update(&bin_bytes);
    hasher.update((canonical_metadata.len() as u64).to_le_bytes());
    hasher.update(canonical_metadata.as_bytes());
    hasher.update(mode_bits.to_le_bytes());

    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn file_mode_bits(path: &Path) -> Result<u32> {
    use std::os::unix::fs::MetadataExt;

    let meta = fs::metadata(path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
        message: format!("failed to stat {}", path.display()),
        source: Box::new(e),
    })?;
    Ok(meta.mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode_bits(_path: &Path) -> Result<u32> {
    Ok(0)
}

pub(crate) fn sort_json_keys(value: &Value) -> Value {
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::external_tools::{SandboxImageRef, SandboxRuntimeSpec};

    #[test]
    fn metadata_deserializes_without_runtime_fields_for_back_compat() {
        let raw = serde_json::json!({
            "tool_abi_version": "1",
            "name": "support/echo",
            "description": "echo",
            "bundle": "support",
            "local_name": "echo",
            "access_level": "read",
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });

        let parsed: ExternalToolMetadata =
            serde_json::from_value(raw).expect("legacy metadata should parse");
        assert!(parsed.runtime.is_none());
        assert!(parsed.runtime_digest.is_none());
    }

    #[test]
    fn metadata_deserializes_sandbox_runtime_with_digest() {
        let raw = serde_json::json!({
            "tool_abi_version": "1",
            "name": "support/sbox",
            "description": "sandbox",
            "bundle": "support",
            "local_name": "sbox",
            "access_level": "read",
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {},
            "runtime": {
                "kind": "sandbox",
                "image": {
                    "kind": "oci",
                    "ref": "ghcr.io/org/tool@sha256:1111111111111111111111111111111111111111111111111111111111111111"
                },
                "entrypoint": ["/app/tool-adapter"]
            },
            "runtime_digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222"
        });

        let parsed: ExternalToolMetadata =
            serde_json::from_value(raw).expect("sandbox metadata should parse");

        match parsed.runtime {
            Some(ToolRuntime::Sandbox(SandboxRuntimeSpec {
                image: SandboxImageRef::Oci { r#ref },
                entrypoint,
                ..
            })) => {
                assert!(r#ref.starts_with("ghcr.io/"));
                assert_eq!(entrypoint, vec!["/app/tool-adapter".to_string()]);
            }
            other => panic!("expected sandbox runtime, got {other:?}"),
        }

        assert_eq!(
            parsed.runtime_digest.as_deref(),
            Some("sha256:2222222222222222222222222222222222222222222222222222222222222222")
        );
    }

    #[test]
    fn build_tool_metadata_pascal_cases_hyphen_and_underscore_components() {
        let raw = serde_json::json!({
            "tool_abi_version": "1",
            "name": "internal-dev/meteo_tool",
            "description": "meteo tool",
            "bundle": "internal-dev",
            "local_name": "meteo_tool",
            "access_level": "read",
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });

        let meta: ExternalToolMetadata = serde_json::from_value(raw).expect("metadata parses");
        let tool_name = ToolName::parse("internal-dev/meteo_tool").expect("valid tool name");
        let built = build_tool_metadata(&meta, &tool_name).expect("metadata builds");

        assert_eq!(built.class_name, "InternalDevMeteoTool");
        assert_eq!(built.input_type.name, "InternalDevMeteoToolInput");
        assert_eq!(built.output_type.name, "InternalDevMeteoToolOutput");
    }

    #[test]
    fn compute_tool_digest_allows_sandbox_without_tool_server() {
        let dir = unique_temp_dir("sandbox-digest-no-bin");
        fs::create_dir_all(&dir).expect("create temp tool dir");

        let metadata = serde_json::json!({
            "tool_abi_version": "1",
            "name": "support/sandbox_only",
            "description": "sandbox only",
            "bundle": "support",
            "local_name": "sandbox_only",
            "access_level": "read",
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {},
            "runtime": {
                "kind": "sandbox",
                "image": {
                    "kind": "bind",
                    "path": "/tmp/sandbox-rootfs"
                },
                "entrypoint": ["/tool-adapter"]
            },
            "runtime_digest": "sha256:3333333333333333333333333333333333333333333333333333333333333333"
        });

        fs::write(
            dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
        )
        .expect("write metadata");

        let digest =
            compute_tool_digest(&dir).expect("sandbox digest should succeed without binary");
        assert!(digest.starts_with("sha256:"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_tool_digest_requires_tool_server_for_process_runtime() {
        let dir = unique_temp_dir("process-digest-missing-bin");
        fs::create_dir_all(&dir).expect("create temp tool dir");

        let metadata = serde_json::json!({
            "tool_abi_version": "1",
            "name": "support/process_only",
            "description": "process only",
            "bundle": "support",
            "local_name": "process_only",
            "access_level": "read",
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });

        fs::write(
            dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
        )
        .expect("write metadata");

        let err = compute_tool_digest(&dir).expect_err("process digest should fail without binary");
        let msg = err.to_string();
        assert!(
            msg.contains("failed to read external tool binary"),
            "unexpected error: {msg}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn compute_tool_digest_for_process_changes_with_binary_mode() {
        let dir = unique_temp_dir("process-digest-mode");
        fs::create_dir_all(&dir).expect("create temp tool dir");

        let metadata = serde_json::json!({
            "tool_abi_version": "1",
            "name": "support/process_mode",
            "description": "process mode",
            "bundle": "support",
            "local_name": "process_mode",
            "access_level": "read",
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });

        fs::write(
            dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
        )
        .expect("write metadata");

        let bin_path = dir.join("tool-server");
        fs::write(&bin_path, b"#!/bin/sh\necho hi\n").expect("write tool-server");

        let mut perms = fs::metadata(&bin_path)
            .expect("stat tool-server")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin_path, perms).expect("chmod 755");
        let digest_755 = compute_tool_digest(&dir).expect("digest 755");

        let mut perms = fs::metadata(&bin_path)
            .expect("stat tool-server")
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&bin_path, perms).expect("chmod 700");
        let digest_700 = compute_tool_digest(&dir).expect("digest 700");

        assert_ne!(
            digest_755, digest_700,
            "mode bits should affect process digest"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_digest_matches_conformance_fixture() {
        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/sandbox-conformance/meteo-tool");
        let metadata_path = fixture_dir.join("tool-metadata.json");
        let expected_path = fixture_dir.join("expected-digests.json");

        let metadata_raw = fs::read_to_string(&metadata_path).expect("read fixture metadata");
        let metadata: ExternalToolMetadata =
            serde_json::from_str(&metadata_raw).expect("parse fixture metadata");

        let expected_raw = fs::read_to_string(&expected_path).expect("read expected digest");
        let expected: serde_json::Value =
            serde_json::from_str(&expected_raw).expect("parse expected digest json");
        let expected_digest = expected
            .get("schema_content_digest")
            .and_then(|v| v.as_str())
            .expect("schema_content_digest string");

        let got = metadata_schema_digest(&metadata);
        assert_eq!(got, expected_digest);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()))
    }
}
