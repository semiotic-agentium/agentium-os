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
