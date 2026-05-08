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

use super::{
    protocol::PROTOCOL_VERSION,
    runtime::{SandboxImageRef, ToolRuntime},
    runtime_lock::read_runtime_lock,
};
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
    /// Invocation semantics (`single_shot` or `session`).
    pub invocation_mode: InvocationMode,
    /// FSM scheduling policy. Defaults to `Strict`.
    #[serde(default)]
    pub session_policy: ExternalSessionPolicy,
    /// Input and output JSON Schemas.
    pub schemas: MetadataSchemas,
    /// Secret names the runtime must resolve for this tool.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Session-mode secret placement (`send` by default).
    #[serde(default)]
    pub secret_scope: ExternalSecretScope,
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
    /// Optional session-coordination BAML declaration. When set, the builder
    /// reads the referenced file from the tool directory and merges its
    /// contents into the agent's generated BAML prelude — equivalent to an
    /// internal tool registering a `SessionCoordinationProvider` via inventory.
    ///
    /// Required for external `invocation_mode=session` tools that expose a
    /// `Choose<Tool>Action` step-executor function to agents. Forbidden for
    /// `single_shot` tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationSpec>,
}

/// Tool-author-supplied coordination BAML pointer.
///
/// The file lives next to `tool-metadata.json` in the tool package and contains
/// the `Choose<Tool>Action` function plus any tool-specific terminal classes
/// (Report / AskUser / etc.). The builder concatenates this into the prelude;
/// auto-generated session classes (Open/Send/.../SessionPlan) come from the
/// JSON schema and are emitted by the builder, not by the tool author.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationSpec {
    /// File name (relative to the tool directory) containing the coordination
    /// BAML fragment.
    pub baml_file: String,
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
            secret_scope: ExternalSecretScope::default(),
            capabilities: Value::Object(Default::default()),
            config_bundle: None,
            runtime: None,
            coordination: None,
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

/// Invocation semantics declared by the tool package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationMode {
    /// Stateless one-request execution (`tool/invoke`).
    SingleShot,
    /// Stateful session protocol (`tool/session_*`).
    Session,
}

/// Where secrets are attached for session-mode tools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSecretScope {
    /// Default: include resolved secrets on each `session_send`.
    #[default]
    Send,
    /// Include resolved secrets once on `session_open`.
    Session,
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

/// Read + validate the authored `<dir>/tool-metadata.json` source only.
///
/// This does not resolve bind paths and does not read/merge the local
/// `tool-metadata.lock.json` sidecar. Use it for builder/codegen, source
/// validation, and any workflow that needs the portable committed metadata.
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

/// Read authored metadata and resolve it into the launch-ready runtime view.
///
/// The committed source file stays portable (relative bind paths, no
/// runtime identity); the lock — written by `sandbox-bind-sync` and gitignored
/// — supplies the canonical absolute bind path.
///
/// Behavior per runtime kind:
/// - `Sandbox { image: Bind { path } }`: relative `path` is resolved against
///   `dir` so the runtime never sees a relative bind path. If a lock with
///   `image_path_abs` is present it overrides the resolved path.
/// - Bind lock supplies only local path resolution; no digest is merged.
/// - `Process` and OCI: lock is ignored.
pub fn read_runtime_external_metadata(dir: &Path) -> Result<ExternalToolMetadata> {
    let mut parsed = read_external_metadata(dir)?;
    apply_runtime_lock(dir, &mut parsed)?;
    Ok(parsed)
}

fn apply_runtime_lock(dir: &Path, meta: &mut ExternalToolMetadata) -> Result<()> {
    let lock = read_runtime_lock(dir)?;

    // Lock semantics are bind-scoped: it carries host-resolved fields that only
    // make sense for `Sandbox::Bind`. OCI sandboxes and process tools must not
    // be influenced by a locally-written lock — their digests live elsewhere
    // (image ref / process binary digest).
    if let Some(ToolRuntime::Sandbox(spec)) = meta.runtime.as_mut()
        && let SandboxImageRef::Bind { path } = &mut spec.image
    {
        // Resolve relative bind paths against the metadata directory so
        // runtime callers never see "./rootfs"-style values, even when
        // the lock is missing.
        if path.is_relative() {
            *path = dir.join(&path);
        }
        if let Some(lock) = &lock {
            if let Some(abs) = &lock.image_path_abs {
                *path = abs.clone();
            }
        }
    }

    Ok(())
}

/// Project parsed metadata into the runtime [`ToolFunctionMetadata`] shape.
///
/// `dir` is the tool package directory containing `tool-metadata.json`. When
/// `meta.coordination` is set, the referenced BAML file is read relative to
/// `dir` and attached as [`ToolFunctionMetadata::coordination_baml`].
pub(crate) fn build_tool_metadata(
    dir: &Path,
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

    let coordination_baml = match &meta.coordination {
        Some(spec) => {
            if matches!(meta.invocation_mode, InvocationMode::SingleShot) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "tool '{}' declares coordination.baml_file but invocation_mode is single_shot; \
                     coordination is only valid for session tools",
                    meta.name
                )));
            }
            if spec.baml_file.is_empty() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "tool '{}' has empty coordination.baml_file",
                    meta.name
                )));
            }
            let coord_path = dir.join(&spec.baml_file);
            let body = fs::read_to_string(&coord_path).map_err(|e| {
                BamlRtError::InvalidArgumentWithSource {
                    message: format!(
                        "tool '{}': failed to read coordination BAML at {}",
                        meta.name,
                        coord_path.display()
                    ),
                    source: Box::new(e),
                }
            })?;
            Some(body)
        }
        None => None,
    };

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
        coordination_baml,
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
/// runtime identity is declared by sandbox metadata (`runtime.image`) rather than host executable bytes.
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
        serde_jcs::to_vec(&metadata_json).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: "failed to canonicalize external tool metadata JSON".to_string(),
            source: Box::new(e),
        })?;

    if matches!(metadata.runtime, Some(ToolRuntime::Sandbox(_))) {
        let mut hasher = Sha256::new();
        hasher.update(b"baml-ext-tool-sandbox-v1\0");
        hasher.update((canonical_metadata.len() as u64).to_le_bytes());
        hasher.update(&canonical_metadata);
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
    hasher.update(&canonical_metadata);
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
        assert_eq!(parsed.secret_scope, ExternalSecretScope::Send);
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
            }
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
        let dir = std::env::temp_dir();
        let built = build_tool_metadata(&dir, &meta, &tool_name).expect("metadata builds");

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
            }
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
