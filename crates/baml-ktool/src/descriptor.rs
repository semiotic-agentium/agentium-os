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
//! Lives in `baml-ktool` so the guest supervisor parses the same type the host
//! audits, with no dependency on a host crate.

use baml_sandbox_protocol::ErrorClass;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::digest::{Digest, canonical_digest};

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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

/// Capabilities a *toolset* requests (data for operator approval, never grants).
///
/// R1 (§16.6, §R1.1): network and secrets are properties of the one MCP server
/// process a `.ktool` ships, not of an individual tool, so this lives on the
/// [`crate::KtoolManifest`], **not** on [`ToolDescriptor`]. `Sandbox`-tier
/// toolsets use only `net` + `secrets`; devices/mounts/GPU mean the toolset
/// belongs in the `Trusted` tier (no granular request type exists, §3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<HostPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretRequest>,
}

impl CapabilityRequest {
    /// True when the tool declared no outbound destinations.
    pub fn has_network(&self) -> bool {
        !self.net.is_empty()
    }
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
    /// JSON Schema. Maps directly to MCP `outputSchema`.
    pub output_schema: Value,
    pub error_classes: Vec<ErrorClass>,
    pub side_effect: SideEffect,
    pub idempotent: bool,
    pub output_trust: OutputTrust,
}

/// Annotation namespace for Karakara fields carried through MCP's free-form
/// `annotations` object.
const NS: &str = "karakara";

impl ToolDescriptor {
    /// Render as an MCP tool definition (`{name, description, inputSchema,
    /// annotations`, `outputSchema`). Karakara-only fields live under
    /// `annotations.karakara.*`, while standard MCP annotations mirror side
    /// effect and idempotency for MCP-native consumers.
    pub fn to_mcp(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "outputSchema": self.output_schema,
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
                },
            },
        })
    }

    /// Parse an MCP tool definition into a descriptor. A bare MCP tool (no
    /// `karakara` annotations) yields safe defaults: its MCP output schema,
    /// side-effect from standard annotations, `[Execution]` error class, and
    /// **Untrusted** output.
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
        let destructive_hint = annotations
            .get("destructiveHint")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let idempotent_hint = annotations.get("idempotentHint").and_then(Value::as_bool);
        let k = annotations.get(NS).cloned().unwrap_or_else(|| json!({}));

        let field = |name: &str| k.get(name).cloned();
        let side_effect: SideEffect = field("sideEffect")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(if read_only_hint {
                SideEffect::Read
            } else if destructive_hint {
                SideEffect::Irreversible
            } else {
                SideEffect::Write
            });
        let output_schema = v
            .get("outputSchema")
            .cloned()
            .or_else(|| field("outputSchema"))
            .unwrap_or_else(|| json!({}));
        let error_classes: Vec<ErrorClass> = field("errorClasses")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_else(|| vec![ErrorClass::Execution]);
        let output_trust: OutputTrust = field("outputTrust")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(OutputTrust::Untrusted);
        let descriptor_abi = field("descriptorAbi")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .unwrap_or(DESCRIPTOR_ABI);
        let version = field("version")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let idempotent = field("idempotent")
            .and_then(|v| v.as_bool())
            .or(idempotent_hint)
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
        })
    }
}

/// Error mapping an MCP tool definition into a [`ToolDescriptor`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpMappingError {
    #[error("MCP tool definition missing required `name`")]
    MissingName,
}

/// The one digest for every tool kind (JCS + SHA-256 over the descriptor
/// *interface*). Static, external, and MCP descriptors all go through this so
/// parity is enforced by construction.
///
/// `version` is deliberately excluded (plan §16.1/§16.7): the descriptor digest
/// covers the interface the agent codegens against, while the toolset version is
/// snapshot/lockfile identity pinned separately. A pure version bump with an
/// unchanged interface must therefore leave this digest unchanged (§16.2: that
/// path is operator-approval only, never an agent rebuild). We normalize the
/// version to empty before hashing so the digest is independent of its value
/// while every other field stays intact; a descriptor whose version is already
/// empty (static tools, bare MCP imports) hashes identically to before.
pub fn compute_descriptor_digest(descriptor: &ToolDescriptor) -> Digest {
    if descriptor.version.is_empty() {
        return canonical_digest(descriptor);
    }
    let mut interface = descriptor.clone();
    interface.version = String::new();
    canonical_digest(&interface)
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
        assert_eq!(mcp["name"], "weather/meteo");
        assert!(mcp["inputSchema"].is_object());
        assert_eq!(mcp["annotations"]["readOnlyHint"], true);
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
        assert_eq!(d.output_trust, OutputTrust::Untrusted);
        assert_eq!(d.error_classes, vec![ErrorClass::Execution]);
        assert_eq!(d.side_effect, SideEffect::Write);
    }

    #[test]
    fn plain_mcp_output_schema_and_annotations_are_preserved() {
        let mcp = json!({
            "name": "delete_record",
            "inputSchema": {"type": "object"},
            "outputSchema": {"type": "object", "properties": {"deleted": {"type": "boolean"}}},
            "annotations": {"destructiveHint": true, "idempotentHint": true},
        });
        let descriptor = ToolDescriptor::from_mcp(&mcp).unwrap();
        assert_eq!(descriptor.side_effect, SideEffect::Irreversible);
        assert!(descriptor.idempotent);
        assert_eq!(descriptor.output_schema, mcp["outputSchema"]);
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
    fn digest_ignores_version() {
        // Plan §16.1/§16.2/§16.7: version is snapshot/lockfile identity, not part
        // of the interface. A pure version bump must not change descriptor_digest
        // (else it would force an unnecessary agent rebuild and drift the live
        // `tools/list` check, which cannot report a version).
        let mut a = sample();
        a.version = "2.0.0".into();
        let mut b = a.clone();
        b.version = "9.9.9".into();
        assert_eq!(
            compute_descriptor_digest(&a),
            compute_descriptor_digest(&b),
            "version must not influence the interface digest"
        );
        let mut empty = a.clone();
        empty.version = String::new();
        assert_eq!(
            compute_descriptor_digest(&a),
            compute_descriptor_digest(&empty),
            "a stamped version must hash identically to the empty-version baseline"
        );
    }
}
