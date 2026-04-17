//! Runtime declarations for external tools (`runtime` block in
//! `tool-metadata.json`).
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

use serde::{Deserialize, Serialize};

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
}

impl ToolRuntime {
    /// Runtime kind discriminant without unpacking the spec.
    pub fn kind(&self) -> ToolRuntimeKind {
        match self {
            Self::Process(_) => ToolRuntimeKind::Process,
            Self::Sandbox(_) => ToolRuntimeKind::Sandbox,
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

/// Sandbox runtime: digest-pinned OCI image executed inside a microVM.
///
/// Digest-pinning is required (§8.4) and is enforced by a schema `if/then`
/// rule landing in Workstream C. Workstream A accepts any string so the type
/// compiles without depending on the schema changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRuntimeSpec {
    /// OCI reference, e.g. `ghcr.io/org/dev-echo@sha256:...`. Tag-only refs
    /// will be rejected at validation time (Workstream C).
    pub image: String,

    /// Guest-side entrypoint argv. Empty => use the image's default
    /// entrypoint. The runner launches the `tool-adapter` via
    /// `microsandbox::Sandbox::exec_stream` with this argv (§5.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoint: Vec<String>,
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
        let json = r#"{"kind":"sandbox","image":"ghcr.io/org/tool@sha256:abc","entrypoint":["/app/tool-adapter"]}"#;
        let rt: ToolRuntime = serde_json::from_str(json).unwrap();
        assert_eq!(rt.kind(), ToolRuntimeKind::Sandbox);
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
