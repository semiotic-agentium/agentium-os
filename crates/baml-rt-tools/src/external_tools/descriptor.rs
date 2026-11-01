// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Unified tool descriptor (external tools rewrite — see
//! `plans/external-tools-v2-p1-spec.md` §P1.1/§P1.2).
//!
//! One reflection object produced by every tool kind — static (`inventory!`),
//! external (`.ktool` bundle), and MCP (imported) — digested by one function
//! ([`compute_descriptor_digest`]) so the three cannot drift. It is a superset
//! of the MCP tool definition and maps losslessly to/from it ([`to_mcp`] /
//! [`from_mcp`]).
//!
//! This supersedes the split `access_level` + free-form `capabilities` +
//! `compute_external_schema_digest`/`compute_manifest_digest` surface. The
//! digest reuses the platform convention (JCS + SHA-256) via
//! [`crate::mcp_snapshot::canonical_digest`].

use baml_sandbox_protocol::ErrorClass;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mcp_snapshot::{Digest, canonical_digest};

/// Descriptor ABI; bump on a breaking shape change.
pub const DESCRIPTOR_ABI: u32 = 1;

/// Side effect of a tool, replacing the old `ToolAccess` (read/write/delete).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// No observable change (safe to retry, safe to parallelise).
    Read,
    /// Mutates external state but recoverable.
    Write,
    /// Not undoable (payments, deletes, sends).
    Irreversible,
}

/// Whether the tool's output may be trusted as structured data or must be
/// treated as untrusted input to the LLM (prompt-injection surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTrust {
    Trusted,
    Untrusted,
}

/// A host:port the tool requests outbound access to. The host resolves this to
/// a CIDR grant at approval time (§8: domain allow-rules are unreliable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

/// How a named secret is bound (§P1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretBinding {
    /// Injected by the host network layer on egress to the approved host;
    /// plaintext never enters the VM. The default.
    EgressBound,
    /// Plaintext delivered to one worker over a guest-internal fd; separately
    /// approved, rare.
    Reveal,
}

/// A secret the tool requests, by name. Values are never in the descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRequest {
    pub name: String,
    pub binding: SecretBinding,
}

/// Capabilities a tool *requests* (data for operator approval, never grants).
/// `Sandbox`-tier tools use only `net` + `secrets`; devices/mounts/GPU mean the
/// tool belongs in the `Trusted` tier (no granular request type exists, §3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<HostPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretRequest>,
}

/// The one descriptor for static, external, and MCP tools.
///
/// `descriptor_digest` is **not** a field: it is computed from the whole struct
/// by [`compute_descriptor_digest`] and carried alongside (snapshot/bundle), so
/// there is no "digest of self minus the digest field" footgun.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub descriptor_abi: u32,
    /// Fully-qualified `bundle/local_name`.
    pub name: String,
    pub version: String,
    pub description: String,
    /// JSON Schema. Equals the MCP `inputSchema`.
    pub input_schema: Value,
    /// JSON Schema. MCP has no output schema; see [`to_mcp`]/[`from_mcp`].
    pub output_schema: Value,
    pub error_classes: Vec<ErrorClass>,
    pub side_effect: SideEffect,
    pub idempotent: bool,
    pub output_trust: OutputTrust,
    pub requested: CapabilityRequest,
}

/// Annotation namespace for Karakara fields carried through MCP's free-form
/// `annotations` object.
const NS: &str = "karakara";

impl ToolDescriptor {
    /// Render as an MCP tool definition (`{name, description, inputSchema,
    /// annotations}`). Everything MCP lacks lives under `annotations.karakara.*`
    /// so a Karakara tool is a valid MCP tool. The standard MCP `readOnlyHint`
    /// mirrors `side_effect == Read` for MCP-native consumers.
    pub fn to_mcp(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": {
                "readOnlyHint": self.side_effect == SideEffect::Read,
                NS: {
                    "descriptorAbi": self.descriptor_abi,
                    "version": self.version,
                    "outputSchema": self.output_schema,
                    "errorClasses": self.error_classes,
                    "sideEffect": self.side_effect,
                    "idempotent": self.idempotent,
                    "outputTrust": self.output_trust,
                    "requested": self.requested,
                },
            },
        })
    }

    /// Parse an MCP tool definition into a descriptor. A bare MCP tool (no
    /// `karakara` annotations) yields safe defaults: empty output schema,
    /// side-effect from `readOnlyHint` (else `Write`), `[Execution]` error
    /// class, **Untrusted** output, and no requested capabilities.
    pub fn from_mcp(v: &Value) -> Result<Self, McpMappingError> {
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .ok_or(McpMappingError::MissingName)?
            .to_string();
        let description = v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let input_schema = v.get("inputSchema").cloned().unwrap_or_else(|| json!({}));
        let annotations = v.get("annotations").cloned().unwrap_or_else(|| json!({}));
        let read_only_hint = annotations
            .get("readOnlyHint")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let k = annotations.get(NS).cloned().unwrap_or_else(|| json!({}));

        let field = |name: &str| k.get(name).cloned();
        let side_effect: SideEffect = field("sideEffect")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(if read_only_hint {
                SideEffect::Read
            } else {
                SideEffect::Write
            });
        let output_schema = field("outputSchema").unwrap_or_else(|| json!({}));
        let error_classes: Vec<ErrorClass> = field("errorClasses")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_else(|| vec![ErrorClass::Execution]);
        let output_trust: OutputTrust = field("outputTrust")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(OutputTrust::Untrusted);
        let requested: CapabilityRequest = field("requested")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let descriptor_abi = field("descriptorAbi")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .unwrap_or(DESCRIPTOR_ABI);
        let version = field("version")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let idempotent = field("idempotent")
            .and_then(|v| v.as_bool())
            .unwrap_or(side_effect == SideEffect::Read);

        Ok(Self {
            descriptor_abi,
            name,
            version,
            description,
            input_schema,
            output_schema,
            error_classes,
            side_effect,
            idempotent,
            output_trust,
            requested,
        })
    }
}

/// Error mapping an MCP tool definition into a [`ToolDescriptor`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpMappingError {
    #[error("MCP tool definition missing required `name`")]
    MissingName,
}

/// The one digest for every tool kind (JCS + SHA-256 over the full descriptor).
/// Static, external, and MCP descriptors all go through this so parity is
/// enforced by construction. Replaces `compute_external_schema_digest` +
/// `compute_manifest_digest`.
pub fn compute_descriptor_digest(descriptor: &ToolDescriptor) -> Digest {
    canonical_digest(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ToolDescriptor {
        ToolDescriptor {
            descriptor_abi: DESCRIPTOR_ABI,
            name: "weather/meteo".into(),
            version: "1.2.0".into(),
            description: "Current weather by coordinates".into(),
            input_schema: json!({"type": "object", "properties": {"latitude": {"type": "number"}}}),
            output_schema: json!({"type": "object", "properties": {"temperature_2m": {"type": "number"}}}),
            error_classes: vec![ErrorClass::InvalidArgument, ErrorClass::Transient],
            side_effect: SideEffect::Read,
            idempotent: true,
            output_trust: OutputTrust::Untrusted,
            requested: CapabilityRequest {
                net: vec![HostPort {
                    host: "api.open-meteo.com".into(),
                    port: 443,
                }],
                secrets: vec![SecretRequest {
                    name: "METEO_KEY".into(),
                    binding: SecretBinding::EgressBound,
                }],
            },
        }
    }

    #[test]
    fn mcp_roundtrip_is_lossless() {
        let d = sample();
        let mcp = d.to_mcp();
        let back = ToolDescriptor::from_mcp(&mcp).unwrap();
        assert_eq!(d, back, "full descriptor must survive the MCP round trip");
    }

    #[test]
    fn karakara_tool_is_a_valid_mcp_tool() {
        let mcp = sample().to_mcp();
        // MCP-native fields present at the top level.
        assert_eq!(mcp["name"], "weather/meteo");
        assert!(mcp["inputSchema"].is_object());
        assert_eq!(mcp["annotations"]["readOnlyHint"], true);
        // Karakara extras namespaced under annotations.karakara.
        assert_eq!(mcp["annotations"]["karakara"]["sideEffect"], "read");
    }

    #[test]
    fn bare_mcp_tool_imports_with_safe_defaults() {
        let bare = json!({
            "name": "search",
            "description": "search the docs",
            "inputSchema": {"type": "object"},
        });
        let d = ToolDescriptor::from_mcp(&bare).unwrap();
        assert_eq!(d.name, "search");
        assert_eq!(d.output_trust, OutputTrust::Untrusted); // MCP output is untrusted
        assert_eq!(d.error_classes, vec![ErrorClass::Execution]);
        assert_eq!(d.side_effect, SideEffect::Write); // no readOnlyHint ⇒ assume effectful
        assert!(d.requested.net.is_empty()); // host grants nothing it didn't ask for
    }

    #[test]
    fn mcp_read_only_hint_maps_to_read() {
        let bare = json!({
            "name": "lookup",
            "inputSchema": {},
            "annotations": {"readOnlyHint": true},
        });
        let d = ToolDescriptor::from_mcp(&bare).unwrap();
        assert_eq!(d.side_effect, SideEffect::Read);
    }

    #[test]
    fn digest_is_stable_and_sensitive() {
        let d = sample();
        let a = compute_descriptor_digest(&d);
        let b = compute_descriptor_digest(&d.clone());
        assert_eq!(a, b, "digest must be deterministic");
        assert!(a.to_string().starts_with("sha256:"));

        let mut changed = d;
        changed.side_effect = SideEffect::Irreversible;
        assert_ne!(
            a,
            compute_descriptor_digest(&changed),
            "a contract change must change the digest"
        );
    }

    #[test]
    fn missing_name_is_an_error() {
        assert_eq!(
            ToolDescriptor::from_mcp(&json!({"description": "x"})),
            Err(McpMappingError::MissingName)
        );
    }
}
