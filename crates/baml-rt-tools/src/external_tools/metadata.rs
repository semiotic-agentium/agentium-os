// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External tool manifest + discovered metadata model.
//!
//! [`ExternalToolManifest`] is the developer-authored `tool-manifest.json`.
//! [`ExternalToolMetadata`] is generated from manifest + discovered schemas and
//! stored in approved snapshots.
//!
//! Downstream machinery (secret requests, session policies, digest helpers)
//! still lives here so there is a single place where the metadata file's
//! semantics are defined.

use std::{fs, path::Path};

use baml_rt_core::{BamlRtError, EventSourceKind, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{EventSchema, protocol::PROTOCOL_VERSION, runtime::ToolRuntime};
use crate::{
    ToolName,
    tools::{
        BundleName, SecretRequest, SessionPolicy, ToolAccess, ToolBackend, ToolFunctionMetadata,
        ToolOrigin, ToolTypeSpec,
    },
};

/// Generated runtime metadata: manifest fields plus discovered schemas.
///
/// Tool authors do not write this directly; snapshots persist it after
/// discovery.
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
    /// Input/output JSON Schemas for callable tools, plus event contracts for datasources.
    pub schemas: MetadataSchemas,
    /// Event kinds this tool may produce through datasource entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_sources: Vec<String>,
    /// Operational datasource declarations from the authored manifest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasources: Vec<ExternalDatasourceManifest>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination_baml: Option<String>,
}

/// Tool-author-supplied coordination BAML pointer.
///
/// The file lives next to `tool-manifest.json` in the tool package and contains
/// the `Choose<Tool>Action` function plus any tool-specific terminal classes
/// (Report / AskUser / etc.). The builder concatenates this into the prelude;
/// auto-generated session classes (Open/Send/.../SessionPlan) come from the
/// JSON schema and are emitted by the builder, not by the tool author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            event_sources: Vec::new(),
            datasources: Vec::new(),
            invocation_mode: InvocationMode::SingleShot,
            session_policy: ExternalSessionPolicy::default(),
            schemas: MetadataSchemas {
                input: input_schema,
                output: output_schema,
                events: Vec::new(),
            },
            secrets: Vec::new(),
            secret_scope: ExternalSecretScope::default(),
            capabilities: Value::Object(Default::default()),
            config_bundle: None,
            runtime: None,
            coordination: None,
            coordination_baml: None,
        }
    }

    /// Set discovery tags on this metadata (builder-style).
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Serialize to pretty JSON with trailing newline, the shape the CLI
    /// writes into generated metadata snapshots.
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

impl InvocationMode {
    /// Map manifest invocation mode to runtime read/stream semantics.
    #[must_use]
    pub fn capability(self) -> crate::tools::ToolCapability {
        match self {
            Self::SingleShot => crate::tools::ToolCapability::OneShot,
            Self::Session => crate::tools::ToolCapability::Streaming,
        }
    }
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

/// Input/output JSON Schemas plus datasource event contracts carried in snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataSchemas {
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub output: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalDatasourceMode {
    Raw,
    Handler,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalDatasourceManifest {
    pub key: String,
    pub kind: String,
    pub mode: ExternalDatasourceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalToolManifest {
    pub tool_abi_version: String,
    pub name: String,
    pub description: String,
    pub bundle: String,
    pub local_name: String,
    pub access_level: ToolAccess,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasources: Vec<ExternalDatasourceManifest>,
    pub invocation_mode: InvocationMode,
    #[serde(default)]
    pub session_policy: ExternalSessionPolicy,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub secret_scope: ExternalSecretScope,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_bundle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<ToolRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationSpec>,
}

impl ExternalToolManifest {
    pub fn into_metadata(self, schemas: MetadataSchemas) -> ExternalToolMetadata {
        ExternalToolMetadata {
            tool_abi_version: self.tool_abi_version,
            name: self.name,
            description: self.description,
            bundle: self.bundle,
            local_name: self.local_name,
            access_level: self.access_level,
            tags: self.tags,
            invocation_mode: self.invocation_mode,
            event_sources: self.event_sources,
            datasources: self.datasources,
            session_policy: self.session_policy,
            schemas,
            secrets: self.secrets,
            secret_scope: self.secret_scope,
            capabilities: self.capabilities,
            config_bundle: self.config_bundle,
            runtime: self.runtime,
            coordination: self.coordination,
            coordination_baml: None,
        }
    }
}

impl From<ExternalToolMetadata> for ExternalToolManifest {
    fn from(meta: ExternalToolMetadata) -> Self {
        Self {
            tool_abi_version: meta.tool_abi_version,
            name: meta.name,
            description: meta.description,
            bundle: meta.bundle,
            local_name: meta.local_name,
            access_level: meta.access_level,
            tags: meta.tags,
            event_sources: meta.event_sources,
            datasources: meta.datasources,
            invocation_mode: meta.invocation_mode,
            session_policy: meta.session_policy,
            secrets: meta.secrets,
            secret_scope: meta.secret_scope,
            capabilities: meta.capabilities,
            config_bundle: meta.config_bundle,
            runtime: meta.runtime,
            coordination: meta.coordination,
        }
    }
}

fn parse_event_sources(tool_name: &str, values: &[String]) -> Result<Vec<EventSourceKind>> {
    values
        .iter()
        .map(|value| {
            EventSourceKind::parse(value).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "external tool '{}' declares invalid event source kind '{}'",
                    tool_name, value
                ))
            })
        })
        .collect()
}

/// Read the tool manifest from `<dir>/tool-manifest.json`.
///
/// Schema discovery and snapshot creation require `tool-manifest.json`.
/// Schemas must come from a live `tool/schema` call and are stored in approved
/// snapshots.
pub fn read_external_manifest(dir: &Path) -> Result<ExternalToolManifest> {
    let manifest_path = dir.join("tool-manifest.json");
    if !manifest_path.exists() {
        return Err(BamlRtError::InvalidArgument(format!(
            "no tool-manifest.json found in {}; create one (without schemas) and implement tool/schema",
            dir.display()
        )));
    }
    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to read {}", manifest_path.display()),
            source: Box::new(e),
        }
    })?;
    let parsed: ExternalToolManifest =
        serde_json::from_str(&raw).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to parse {}", manifest_path.display()),
            source: Box::new(e),
        })?;
    validate_external_abi(&parsed.name, &parsed.tool_abi_version)?;
    Ok(parsed)
}

fn validate_external_abi(name: &str, abi: &str) -> Result<()> {
    if abi != PROTOCOL_VERSION {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool '{}' declares unsupported ABI version '{}' (expected '{}')",
            name, abi, PROTOCOL_VERSION
        )));
    }
    Ok(())
}

/// Project parsed metadata into the runtime [`ToolFunctionMetadata`] shape.
///
/// `dir` is the tool package directory containing `tool-manifest.json`. When
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

    let coordination_baml = match (&meta.coordination_baml, &meta.coordination) {
        (Some(body), _) => Some(body.clone()),
        (None, Some(spec)) => {
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
        (None, None) => None,
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
        capability: meta.invocation_mode.capability(),
        event_sources: parse_event_sources(&meta.name, &meta.event_sources)?,
        coordination_baml,
    })
}

/// RFC 8785 (JCS) canonical SHA-256 of the input+output schemas.
///
/// Both runner and tool author must compute this identically for
/// describe-mismatch detection to work.
pub(crate) fn metadata_schema_digest(meta: &ExternalToolMetadata) -> String {
    if meta.datasources.is_empty() {
        schema_digest_from_io(&meta.schemas.input, &meta.schemas.output)
    } else {
        schema_digest_from_events(&meta.schemas.events)
    }
}

/// JCS-canonical SHA-256 over a raw `{input, output}` schema pair.
///
/// Identical hashing to [`metadata_schema_digest`], but taking the schemas
/// directly. Used by the runtime drift guard to recompute a live tool's schema
/// digest from a `tool/schema` response without trusting the tool's
/// self-reported `content_digest`.
pub(crate) fn schema_digest_from_io(
    input: &serde_json::Value,
    output: &serde_json::Value,
) -> String {
    let payload = serde_json::json!({
        "input": input,
        "output": output,
    });
    digest_canonical_payload(&payload)
}

/// JCS-canonical SHA-256 over datasource event contracts.
pub(crate) fn schema_digest_from_events(events: &[EventSchema]) -> String {
    let payload = serde_json::json!({
        "events": events,
    });
    digest_canonical_payload(&payload)
}

fn digest_canonical_payload(payload: &serde_json::Value) -> String {
    let canonical = serde_jcs::to_vec(payload)
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
/// - canonicalized manifest bytes prefixed by u64 little-endian length
/// - filesystem mode bits (`stat().mode() & 0o7777`) as u32 little-endian
///
/// Sandbox runtime input (in order):
/// - magic/version marker: `baml-ext-tool-sandbox-v1\0`
/// - canonicalized manifest bytes prefixed by u64 little-endian length
pub fn compute_tool_digest(dir: &Path) -> Result<String> {
    let manifest_path = dir.join("tool-manifest.json");
    let manifest_raw =
        fs::read_to_string(&manifest_path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to read external tool manifest {}",
                manifest_path.display()
            ),
            source: Box::new(e),
        })?;
    let manifest_json: Value = serde_json::from_str(&manifest_raw).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to parse external tool manifest {}",
                manifest_path.display()
            ),
            source: Box::new(e),
        }
    })?;
    let manifest: ExternalToolManifest = serde_json::from_str(&manifest_raw).map_err(|e| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "failed to decode typed external tool manifest {}",
                manifest_path.display()
            ),
            source: Box::new(e),
        }
    })?;
    validate_external_abi(&manifest.name, &manifest.tool_abi_version)?;
    let canonical_manifest =
        serde_jcs::to_vec(&manifest_json).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: "failed to canonicalize external tool manifest JSON".to_string(),
            source: Box::new(e),
        })?;

    if matches!(manifest.runtime, Some(ToolRuntime::Sandbox(_))) {
        let mut hasher = Sha256::new();
        hasher.update(b"baml-ext-tool-sandbox-v1\0");
        hasher.update((canonical_manifest.len() as u64).to_le_bytes());
        hasher.update(&canonical_manifest);
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
    hasher.update((canonical_manifest.len() as u64).to_le_bytes());
    hasher.update(&canonical_manifest);
    hasher.update(mode_bits.to_le_bytes());
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

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
