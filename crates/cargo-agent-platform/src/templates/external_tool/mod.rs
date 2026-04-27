//! Templates for external tool scaffolds.
//!
//! The generator is parameterised over a typed [`ScaffoldContext`] and a
//! [`Language`] enum; everything stringly-typed has been lifted into proper
//! Rust types so the compiler can enforce exhaustive handling.

pub mod bind_sandbox;
pub mod lang;
pub mod metadata_json;
pub mod readme_md;

use baml_rt_tools::{ToolAccess, external_tools::SandboxImageRef};
use clap::ValueEnum;

/// File emitted by an external-tool language template.
#[derive(Debug, Clone)]
pub struct GeneratedFile {
    pub relative_path: String,
    pub content: String,
    pub executable: bool,
}

impl GeneratedFile {
    pub fn new(relative_path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            content: content.into(),
            executable: false,
        }
    }

    pub fn executable(mut self) -> Self {
        self.executable = true;
        self
    }
}

/// Default bundle namespace when the user doesn't supply `--bundle`.
///
/// `support/` is the convention for business integrations (ClickUp, Notion,
/// Slack, calculators). The runtime accepts any non-empty, slash-free string
/// via [`baml_rt_tools::BundleName`], so we keep this field free-form rather
/// than closed enum — scaffolds like `travel/` or `acme/` work without
/// code changes.
pub const DEFAULT_BUNDLE: &str = "support";

// ----- Starter scaffold contract constants -----
//
// The scaffold ships a trivial "echo" tool so the user can invoke and verify
// the protocol end-to-end immediately. These constants are the *single source
// of truth* for that contract. Rename them here and every template (plus the
// JSON schema in tool-metadata.json and the README probe example) picks up
// the change — no risk of divergence between the schema and the handler.

/// Name of the starter input field. Handlers read it from `params.input.<KEY>`.
///
/// Scaffolder-only: this is a starter-template default, not runtime truth.
/// Different tools will pick different schemas; this key is what the
/// generated echo scaffold uses so the probe example in the README "just works".
pub const STARTER_INPUT_KEY: &str = "message";

/// Name of the starter output field. Handlers echo into `result.output.<KEY>`.
/// See [`STARTER_INPUT_KEY`] for why this lives in the scaffolder.
pub const STARTER_OUTPUT_KEY: &str = "echoed";

/// CLI-facing wrapper around [`ToolAccess`].
///
/// Exists only so clap can derive [`ValueEnum`] — the runtime's `ToolAccess`
/// cannot take a clap dependency without bleeding CLI concerns into
/// `baml-rt-tools`. Every value round-trips losslessly via [`From`]; there is
/// no semantic difference, only the derive surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Access {
    Read,
    Write,
    Delete,
}

impl From<Access> for ToolAccess {
    fn from(access: Access) -> Self {
        match access {
            Access::Read => ToolAccess::Read,
            Access::Write => ToolAccess::Write,
            Access::Delete => ToolAccess::Delete,
        }
    }
}

impl Access {
    /// Delegates to [`ToolAccess::as_str`] so there is only one source of truth
    /// for the canonical lowercase spelling used in `tool-metadata.json`.
    pub fn as_str(self) -> &'static str {
        ToolAccess::from(self).as_str()
    }
}

/// Runtime target encoded in scaffolded `tool-metadata.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Runtime {
    Process,
    Sandbox,
}

/// Invocation mode encoded in scaffolded `tool-metadata.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InvocationMode {
    SingleShot,
    Session,
}

/// Sandbox source type when `runtime=sandbox`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SandboxSource {
    Oci,
    Bind,
}

impl Runtime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Sandbox => "sandbox",
        }
    }
}

impl InvocationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingleShot => "single_shot",
            Self::Session => "session",
        }
    }
}

/// Language the tool is authored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Language {
    Rust,
    Bash,
    Python,
    Typescript,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Bash => "bash",
            Self::Python => "python",
            Self::Typescript => "typescript",
        }
    }

    /// One-liner printed in the README under "Local setup".
    pub fn setup_block(self) -> &'static str {
        match self {
            Self::Rust => {
                "# Debug build (fast iteration):\ncargo run --quiet --manifest-path ./Cargo.toml -- </dev/null || true\n\n# Release build (faster tool invoke after first agent boot):\ncargo build --release --manifest-path ./Cargo.toml"
            }
            Self::Bash => "./tool-server </dev/null || true",
            Self::Python => "python3 ./main.py </dev/null || true",
            Self::Typescript => "npm install\nnpm run build",
        }
    }

    /// Dispatch to the language-specific scaffold.
    pub fn files(self, ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
        match self {
            Self::Rust => lang::rust::generate(ctx),
            Self::Bash => lang::bash::generate(ctx),
            Self::Python => lang::python::generate(ctx),
            Self::Typescript => lang::typescript::generate(ctx),
        }
    }

    /// Default `(command, workdir)` pair the adapter should invoke for
    /// `tool/invoke` in a sandbox. Single source of truth for scaffold-time
    /// `runtime.adapter` defaults and for the adapter shim's baked-in
    /// `FALLBACK_RUNTIME` — keeping them aligned avoids silent drift.
    pub fn default_adapter_command(self) -> (&'static [&'static str], &'static str) {
        match self {
            Self::Python => (&["python3", "/opt/tool/main.py"], "/opt/tool"),
            Self::Bash => (&["/opt/tool/tool-server"], "/opt/tool"),
            Self::Rust => (&["/opt/tool/external-tool"], "/opt/tool"),
            Self::Typescript => (&["node", "/opt/tool/dist/main.js"], "/opt/tool"),
        }
    }
}

/// Inputs passed to every language scaffold so signatures stay uniform.
#[derive(Debug, Clone)]
pub struct ScaffoldContext<'a> {
    pub name: &'a str,
    /// Bundle namespace — free-form string (e.g. `support`, `travel`, `acme`).
    /// Validation happens via [`baml_rt_tools::BundleName`] at the call site
    /// so this scaffold layer stays agnostic to the runtime's bundle model.
    pub bundle: &'a str,
    pub access: Access,
    pub language: Language,
    pub description: &'a str,
    /// Runtime metadata block to emit in `tool-metadata.json`.
    pub runtime: Runtime,
    /// Invocation mode to emit in metadata (`single_shot` or `session`).
    pub invocation_mode: InvocationMode,
    /// Sandbox source when runtime is sandbox.
    pub sandbox_source: Option<SandboxSource>,
    /// Sandbox image reference/path when runtime is sandbox.
    pub sandbox_image: Option<SandboxImageRef>,
    /// Runtime identity digest (`sha256:...`) when runtime is sandbox.
    pub runtime_digest: Option<String>,
    /// Optional sandbox entrypoint argv.
    pub sandbox_entrypoint: Vec<String>,
    /// Whether to scaffold Docker-oriented bind helper artifacts.
    ///
    /// Only meaningful for `runtime=sandbox` + `sandbox_source=bind`.
    pub generate_docker: bool,
}

impl<'a> ScaffoldContext<'a> {
    pub fn tool_id(&self) -> String {
        format!("{}/{}", self.bundle, self.name)
    }
}

/// Emit the shared `tool-server` bash launcher that `cd`s into the scaffold
/// directory and execs the language-specific entry point.
///
/// Only the `exec` line differs per language, so this helper keeps the
/// wrapper in one place.
pub fn tool_server_wrapper(exec_line: &str) -> GeneratedFile {
    let script = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\nDIR=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\nexec {exec_line}\n"
    );
    GeneratedFile::new("tool-server", script).executable()
}
