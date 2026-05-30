// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `check-external-tool` subcommand implementation.
//!
//! Validates `tool-metadata.json` against the canonical JSON schema and the
//! runtime's typed parser. This catches drift between scaffolded metadata,
//! schema rules, and runtime expectations.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use baml_rt_tools::external_tools::{
    SandboxImageRef, ToolRuntime, read_external_metadata, read_runtime_external_metadata,
};
use console::style;
use jsonschema::JSONSchema;
use serde_json::Value;

use crate::workspace::find_workspace_root;

pub fn run(path: &str) -> Result<()> {
    let tool_dir = Path::new(path);
    if !tool_dir.exists() {
        bail!("tool directory does not exist: {}", tool_dir.display());
    }

    let metadata_path = tool_dir.join("tool-metadata.json");
    let raw = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let instance: Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {} as JSON", metadata_path.display()))?;

    let workspace_root = find_workspace_root()?;
    let schema_path = workspace_root.join("schemas/external_tool_metadata.schema.json");
    let schema_raw = fs::read_to_string(&schema_path)
        .with_context(|| format!("failed to read schema at {}", schema_path.display()))?;
    let schema_json: Value = serde_json::from_str(&schema_raw)
        .with_context(|| format!("failed to parse schema at {}", schema_path.display()))?;

    let compiled = JSONSchema::compile(&schema_json)
        .map_err(|e| anyhow::anyhow!("failed to compile schema {}: {e}", schema_path.display()))?;

    if let Err(errors) = compiled.validate(&instance) {
        println!("{} schema validation failed:", style("✗").red());
        for err in errors {
            println!("  - {err}");
        }
        bail!("external tool metadata failed schema validation");
    }

    // Source-pollution lint: catch contributors who hand-edited absolute bind
    // paths into the committed `tool-metadata.json`. Host-resolved bind paths
    // belong in the gitignored `tool-metadata.lock.json` written by
    // `sandbox-bind-sync`. Run against the unmerged source — the resolved view
    // would mask abs paths that the lock happened to override.
    let source = read_external_metadata(tool_dir)?;
    if let Some(ToolRuntime::Sandbox(spec)) = &source.runtime
        && let SandboxImageRef::Bind { path } = &spec.image
        && path.is_absolute()
    {
        bail!(
            "source tool-metadata.json declares an absolute bind path ({}); use a relative path \
             like \"./.tmp/<rootfs>\" — host-resolved paths belong in tool-metadata.lock.json",
            path.display()
        );
    }
    let typed = read_runtime_external_metadata(tool_dir)?;
    if let Some(ToolRuntime::Sandbox(runtime)) = &typed.runtime {
        let adapter = runtime.adapter.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "sandbox runtime requires runtime.adapter with command/protocol (tool: {})",
                typed.name
            )
        })?;
        if adapter.command.is_empty() {
            bail!(
                "sandbox runtime.adapter.command must contain at least one argv token (tool: {})",
                typed.name
            );
        }
        if adapter.protocol != "jsonrpc-stdio" {
            bail!(
                "sandbox runtime.adapter.protocol must be 'jsonrpc-stdio' (tool: {}, got: {})",
                typed.name,
                adapter.protocol
            );
        }

        match &runtime.image {
            SandboxImageRef::Oci { r#ref } => {
                let Some((_, digest)) = r#ref.split_once("@") else {
                    bail!("sandbox oci image must be digest-pinned (missing @sha256:): {ref}", ref = r#ref);
                };
                if !digest.starts_with("sha256:") {
                    bail!("sandbox oci image must include @sha256:<64-hex>: {ref}", ref = r#ref);
                }
            }
            SandboxImageRef::Bind { path } => {
                let canonical = std::fs::canonicalize(path)
                    .with_context(|| format!("bind path does not resolve: {}", path.display()))?;
                if !canonical.is_dir() {
                    bail!("bind path is not a directory: {}", canonical.display());
                }

                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let mode = std::fs::metadata(&canonical)?.mode() & 0o7777;
                    if mode & 0o002 != 0 {
                        println!(
                            "{} bind rootfs is world-writable (mode {:o}): {}",
                            style("!").yellow(),
                            mode,
                            canonical.display()
                        );
                    }
                }
            }
            _ => {
                bail!(
                    "unsupported sandbox image kind in metadata for tool {}",
                    typed.name
                );
            }
        }
    }

    let runtime_kind = typed
        .runtime
        .as_ref()
        .map(|rt| rt.kind())
        .map(|kind| match kind {
            baml_rt_tools::external_tools::ToolRuntimeKind::Process => "process",
            baml_rt_tools::external_tools::ToolRuntimeKind::Sandbox => "sandbox",
        })
        .unwrap_or("process(default)");

    println!(
        "{} metadata valid for {} ({runtime_kind})",
        style("✓").green(),
        style(&typed.name).cyan()
    );

    if let Some(spec) = &typed.coordination {
        if matches!(
            typed.invocation_mode,
            baml_rt_tools::external_tools::InvocationMode::SingleShot
        ) {
            bail!(
                "tool '{}' declares coordination.baml_file but invocation_mode is single_shot; coordination is only valid for session tools",
                typed.name
            );
        }
        if spec.baml_file.is_empty() {
            bail!("tool '{}' has empty coordination.baml_file", typed.name);
        }
        let coord_path = tool_dir.join(&spec.baml_file);
        if !coord_path.is_file() {
            bail!(
                "tool '{}': coordination file not found at {}",
                typed.name,
                coord_path.display()
            );
        }
        let coord_body = fs::read_to_string(&coord_path).with_context(|| {
            format!(
                "failed to read coordination BAML at {}",
                coord_path.display()
            )
        })?;
        if coord_body.trim().is_empty() {
            bail!(
                "tool '{}': coordination file is empty: {}",
                typed.name,
                coord_path.display()
            );
        }
        println!(
            "{} coordination BAML loaded from {}",
            style("✓").green(),
            style(coord_path.display()).cyan()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commands::new_tool::build_file_set,
        templates::external_tool::{
            Access, InvocationMode, Language, Runtime, SandboxSource, ScaffoldContext,
        },
    };

    #[test]
    fn scaffolded_metadata_passes_runtime_validator() {
        let ctx = ScaffoldContext {
            name: "echo",
            bundle: "dev",
            access: Access::Read,
            language: Language::Bash,
            description: "Echo external tool",
            runtime: Runtime::Process,
            invocation_mode: InvocationMode::SingleShot,
            sandbox_source: Some(SandboxSource::Oci),
            sandbox_image: None,
            sandbox_entrypoint: Vec::new(),
            generate_docker: false,
        };

        let files = build_file_set(&ctx);
        let tmp = tempfile::tempdir().expect("tmp dir");

        for file in files {
            if file.relative_path == "tool-metadata.json" {
                std::fs::write(tmp.path().join(file.relative_path), file.content.as_bytes())
                    .expect("write metadata");
                break;
            }
        }

        run(tmp.path().to_str().expect("utf8 path")).expect("validator should pass scaffold");
    }
}
