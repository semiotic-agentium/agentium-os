// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Runtime declarations for external tools (`runtime` block in
//! `tool-manifest.json`).
//!
//! Workstream A of `tool_sandbox.md`: introduce the typed model with no
//! behavior change. Dispatch by `ToolRuntime` kind lands in Workstream B when
//! [`SandboxInvoker`](super::invoker) is added alongside the process path.
//!
//! Compatibility rules (from `tool_sandbox.md` §4.2):
//! 1. `runtime` is optional on [`super::metadata::ExternalToolMetadata`].
//! 2. Missing `runtime` => process mode with wrapper default
//!    (`./tool-server`).
//! 3. Relative paths resolve against the metadata directory (handled by the
//!    resolver, not this module).

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use tracing::warn;

/// Default command used when a `process` runtime omits `command`. Matches the
/// wrapper shipped by the CLI scaffolder (§4.2 "wrapper kept as default").
pub const DEFAULT_PROCESS_COMMAND: &str = "./tool-server";

/// Tagged runtime declaration. Serialized as `{ "kind": "process", ... }` /
/// `{ "kind": "sandbox", ... }` to match the metadata schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolRuntime {
    /// Current process + stdio JSON-RPC path.
    Process(ProcessRuntimeSpec),
    /// Microsandbox-backed microVM path (dispatch added in Workstream B).
    Sandbox(SandboxRuntimeSpec),
    /// v2 escape hatch (§3.2): an unsandboxed, operator-trusted tool that runs
    /// as a crash-isolated host process over the P4 wire. No capability grants.
    Trusted(TrustedRuntimeSpec),
    /// v2 escape hatch (§3.2): a reference to an endpoint the operator already
    /// runs (MCP-first). The OS holds a client; it runs nothing.
    Service(ServiceRuntimeSpec),
}

impl ToolRuntime {
    /// Runtime kind discriminant without unpacking the spec.
    pub fn kind(&self) -> ToolRuntimeKind {
        match self {
            Self::Process(_) => ToolRuntimeKind::Process,
            Self::Sandbox(_) => ToolRuntimeKind::Sandbox,
            Self::Trusted(_) => ToolRuntimeKind::Trusted,
            Self::Service(_) => ToolRuntimeKind::Service,
        }
    }
}

impl Default for ToolRuntime {
    fn default() -> Self {
        Self::Process(ProcessRuntimeSpec::default())
    }
}

/// Lightweight discriminant useful for match-on-kind without cloning specs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRuntimeKind {
    Process,
    Sandbox,
    Trusted,
    Service,
}

/// Process runtime: spawn a child process; stdio carries the JSON-RPC contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRuntimeSpec {
    /// argv for the child process. Direct exec — no shell expansion. For shell
    /// features, use `["bash", "-c", "..."]`.
    #[serde(default = "default_process_command")]
    pub command: Vec<String>,

    /// Optional deploy-time setup commands (idempotent). Sandbox kind ignores
    /// this field entirely (§4.2 "setup hooks apply to process kind only").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<String>,
}

impl Default for ProcessRuntimeSpec {
    fn default() -> Self {
        Self {
            command: default_process_command(),
            setup: Vec::new(),
        }
    }
}

fn default_process_command() -> Vec<String> {
    vec![DEFAULT_PROCESS_COMMAND.to_string()]
}

/// Sandbox rootfs source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SandboxImageRef {
    /// Digest-pinned OCI reference (`...@sha256:...`).
    Oci { r#ref: String },
    /// Host directory used directly as guest rootfs.
    Bind { path: PathBuf },
}

/// Adapter-side runtime dispatch declaration for sandbox tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxAdapterRuntimeSpec {
    /// Sidecar schema version written under `tool-bundle.json.runtime`.
    #[serde(default = "default_adapter_schema_version")]
    pub schema_version: u32,
    /// Child-protocol spoken by the tool process behind `/tool-adapter`.
    #[serde(default = "default_adapter_protocol")]
    pub protocol: String,
    /// Child process argv invoked by the adapter for `tool/invoke`.
    pub command: Vec<String>,
    /// Optional working directory for the tool child process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
}

fn default_adapter_schema_version() -> u32 {
    1
}

fn default_adapter_protocol() -> String {
    "jsonrpc-stdio".to_string()
}

/// Sandbox runtime: OCI or bind rootfs executed inside a microVM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRuntimeSpec {
    /// Rootfs source. Back-compat accepts the legacy string form and maps it
    /// to `{"kind":"oci","ref":...}` with a deprecation warning.
    #[serde(deserialize_with = "deserialize_sandbox_image_ref")]
    pub image: SandboxImageRef,

    /// Guest-side entrypoint argv. Empty => use the image's default
    /// entrypoint. The runner launches the `tool-adapter` via
    /// `microsandbox::Sandbox::exec_stream` with this argv (§5.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoint: Vec<String>,

    /// Adapter dispatch config authored in metadata and materialized under
    /// `/etc/karakara/tool-bundle.json` during bind sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<SandboxAdapterRuntimeSpec>,
}

/// Trusted runtime (§3.2): unsandboxed host process, operator-trusted. The OS
/// does not mediate its host access; the only authority is the operator-trust
/// record. Capability requests in the descriptor are informational here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedRuntimeSpec {
    /// How the OS obtains the trusted process on its pinned node.
    pub launch: TrustedLaunch,
    /// Node the trusted tool is pinned to; the deploy reachability gate (§5.3)
    /// fails an agent that pins this tool onto an unreachable node. `None`
    /// means "the local node", used by `karak tool test` and single-node OSes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// How a `Trusted` tool's process comes to exist on its node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TrustedLaunch {
    /// The OS spawns the process (blessed DX; `karak publish` installs it).
    Spawn {
        /// Direct exec — no shell. argv[0] is the program.
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
        /// Host env names allowed through to the process (values pass through
        /// from the operator's environment; never the agent's).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        env_allowlist: Vec<String>,
    },
    /// Attach to an operator-run daemon listening on a host-local stream socket.
    Attach { socket_path: PathBuf },
}

/// Service runtime (§3.2): a reference to an endpoint the operator already runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRuntimeSpec {
    /// The endpoint the OS holds a client to. MCP-first in P5.
    pub endpoint: ServiceEndpoint,
}

/// A `Service` endpoint reference. Resolved for reachability at deploy (§5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServiceEndpoint {
    /// An MCP server the operator runs. The OS imports the descriptor and holds
    /// an MCP client (reuses the existing MCP import path).
    Mcp { url: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SandboxImageRefCompat {
    LegacyOciString(String),
    Tagged(SandboxImageRef),
}

fn deserialize_sandbox_image_ref<'de, D>(deserializer: D) -> Result<SandboxImageRef, D::Error>
where
    D: Deserializer<'de>,
{
    let compat = SandboxImageRefCompat::deserialize(deserializer)?;
    Ok(match compat {
        SandboxImageRefCompat::LegacyOciString(s) => {
            warn!(
                "deprecated sandbox runtime.image string form used; prefer image={{kind:'oci',ref:'...'}}"
            );
            SandboxImageRef::Oci { r#ref: s }
        }
        SandboxImageRefCompat::Tagged(image) => image,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_runtime_is_process_with_wrapper() {
        let rt = ToolRuntime::default();
        match rt {
            ToolRuntime::Process(spec) => {
                assert_eq!(spec.command, vec![DEFAULT_PROCESS_COMMAND.to_string()]);
                assert!(spec.setup.is_empty());
            }
            _ => panic!("default should be process"),
        }
    }

    #[test]
    fn process_runtime_roundtrips() {
        let json = r#"{"kind":"process","command":["./tool-server"]}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        assert_eq!(rt.kind(), ToolRuntimeKind::Process);
        let back = serde_json::to_string(&rt).unwrap();
        assert!(back.contains(r#""kind":"process""#));
    }

    #[test]
    fn sandbox_runtime_roundtrips() {
        let json = r#"{"kind":"sandbox","image":{"kind":"oci","ref":"ghcr.io/org/tool@sha256:abc"},"entrypoint":["/app/tool-adapter"]}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        assert_eq!(rt.kind(), ToolRuntimeKind::Sandbox);
    }

    #[test]
    fn sandbox_runtime_accepts_legacy_oci_image_string() {
        let json = r#"{"kind":"sandbox","image":"ghcr.io/org/tool@sha256:abc"}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        match rt {
            ToolRuntime::Sandbox(SandboxRuntimeSpec {
                image: SandboxImageRef::Oci { r#ref },
                ..
            }) => assert!(r#ref.contains("@sha256:")),
            other => panic!("expected sandbox oci image, got {other:?}"),
        }
    }

    #[test]
    fn sandbox_runtime_bind_roundtrips() {
        let json = r#"{"kind":"sandbox","image":{"kind":"bind","path":"./rootfs"}}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        match rt {
            ToolRuntime::Sandbox(SandboxRuntimeSpec {
                image: SandboxImageRef::Bind { path },
                ..
            }) => assert_eq!(path, PathBuf::from("./rootfs")),
            other => panic!("expected bind image, got {other:?}"),
        }
    }

    #[test]
    fn sandbox_runtime_adapter_roundtrips() {
        let json = r#"{"kind":"sandbox","image":{"kind":"bind","path":"./rootfs"},"adapter":{"command":["python3","/opt/tool/main.py"],"workdir":"/opt/tool","protocol":"jsonrpc-stdio"}}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        match rt {
            ToolRuntime::Sandbox(SandboxRuntimeSpec {
                adapter: Some(adapter),
                ..
            }) => {
                assert_eq!(adapter.command, vec!["python3", "/opt/tool/main.py"]);
                assert_eq!(adapter.workdir.as_deref(), Some("/opt/tool"));
                assert_eq!(adapter.protocol, "jsonrpc-stdio");
                assert_eq!(adapter.schema_version, 1);
            }
            other => panic!("expected sandbox adapter, got {other:?}"),
        }
    }

    #[test]
    fn trusted_runtime_roundtrips_spawn() {
        let json = r#"{"kind":"trusted","launch":{"mode":"spawn","argv":["node","tool.js"]},"node":"gpu-1"}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        assert_eq!(rt.kind(), ToolRuntimeKind::Trusted);
        match rt {
            ToolRuntime::Trusted(TrustedRuntimeSpec {
                launch: TrustedLaunch::Spawn { argv, .. },
                node,
            }) => {
                assert_eq!(argv, vec!["node", "tool.js"]);
                assert_eq!(node.as_deref(), Some("gpu-1"));
            }
            other => panic!("expected trusted spawn, got {other:?}"),
        }
        let back =
            serde_json::to_string(&serde_json::from_str::<ToolRuntime>(json).unwrap()).unwrap();
        assert!(back.contains(r#""kind":"trusted""#));
        assert!(back.contains(r#""mode":"spawn""#));
    }

    #[test]
    fn trusted_runtime_roundtrips_attach() {
        let json =
            r#"{"kind":"trusted","launch":{"mode":"attach","socket_path":"/run/tool.sock"}}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        match rt {
            ToolRuntime::Trusted(TrustedRuntimeSpec {
                launch: TrustedLaunch::Attach { socket_path },
                node,
            }) => {
                assert_eq!(socket_path, PathBuf::from("/run/tool.sock"));
                assert!(node.is_none());
            }
            other => panic!("expected trusted attach, got {other:?}"),
        }
    }

    #[test]
    fn service_runtime_roundtrips_mcp() {
        let json = r#"{"kind":"service","endpoint":{"kind":"mcp","url":"http://localhost:7000"}}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        assert_eq!(rt.kind(), ToolRuntimeKind::Service);
        match rt {
            ToolRuntime::Service(ServiceRuntimeSpec {
                endpoint: ServiceEndpoint::Mcp { url },
            }) => assert_eq!(url, "http://localhost:7000"),
            other => panic!("expected service mcp, got {other:?}"),
        }
    }

    #[test]
    fn process_runtime_defaults_fill_in_missing_command() {
        let json = r#"{"kind":"process"}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        match rt {
            ToolRuntime::Process(spec) => {
                assert_eq!(spec.command, vec![DEFAULT_PROCESS_COMMAND.to_string()]);
            }
            _ => panic!("expected process"),
        }
    }
}
