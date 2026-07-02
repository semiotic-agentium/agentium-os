// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `check-external-tool` subcommand implementation.
//!
//! Validates `tool-manifest.json` with the runtime typed parser and
//! runtime-specific invariants. Schemas are discovered from `tool/schema` by
//! enable/allowed-dir discovery, not authored in this file.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use baml_rt_tools::external_tools::{
    InvocationMode, SandboxImageRef, ToolRuntime, read_external_manifest, read_runtime_lock,
};
use console::style;

pub fn run(path: &str) -> Result<()> {
    let tool_dir = Path::new(path);
    if !tool_dir.exists() {
        bail!("tool directory does not exist: {}", tool_dir.display());
    }

    let manifest = read_external_manifest(tool_dir)?;

    // Source-pollution lint: catch absolute bind paths in committed source.
    // Host-resolved bind paths belong in gitignored `tool-manifest.lock.json`.
    if let Some(ToolRuntime::Sandbox(spec)) = &manifest.runtime
        && let SandboxImageRef::Bind { path } = &spec.image
        && path.is_absolute()
    {
        bail!(
            "source tool-manifest.json declares an absolute bind path ({}); use a relative path \
             like \"./.tmp/<rootfs>\" — host-resolved paths belong in tool-manifest.lock.json",
            path.display()
        );
    }

    if let Some(ToolRuntime::Sandbox(runtime)) = &manifest.runtime {
        let adapter = runtime.adapter.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "sandbox runtime requires runtime.adapter with command/protocol (tool: {})",
                manifest.name
            )
        })?;
        if adapter.command.is_empty() {
            bail!(
                "sandbox runtime.adapter.command must contain at least one argv token (tool: {})",
                manifest.name
            );
        }
        if adapter.protocol != "jsonrpc-stdio" {
            bail!(
                "sandbox runtime.adapter.protocol must be 'jsonrpc-stdio' (tool: {}, got: {})",
                manifest.name,
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
                let lock = read_runtime_lock(tool_dir)?;
                let resolved = lock
                    .and_then(|lock| lock.image_path_abs)
                    .unwrap_or_else(|| {
                        if path.is_relative() {
                            tool_dir.join(path)
                        } else {
                            path.clone()
                        }
                    });
                let canonical = std::fs::canonicalize(&resolved).with_context(|| {
                    format!("bind path does not resolve: {}", resolved.display())
                })?;
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
                    "unsupported sandbox image kind in manifest for tool {}",
                    manifest.name
                );
            }
        }
    }

    let runtime_kind = manifest
        .runtime
        .as_ref()
        .map(|rt| rt.kind())
        .map(|kind| match kind {
            baml_rt_tools::external_tools::ToolRuntimeKind::Process => "process",
            baml_rt_tools::external_tools::ToolRuntimeKind::Sandbox => "sandbox",
        })
        .unwrap_or("process(default)");

    println!(
        "{} manifest valid for {} ({runtime_kind})",
        style("✓").green(),
        style(&manifest.name).cyan()
    );

    if let Some(spec) = &manifest.coordination {
        if matches!(manifest.invocation_mode, InvocationMode::SingleShot) {
            bail!(
                "tool '{}' declares coordination.baml_file but invocation_mode is single_shot; coordination is only valid for session tools",
                manifest.name
            );
        }
        if spec.baml_file.is_empty() {
            bail!("tool '{}' has empty coordination.baml_file", manifest.name);
        }
        let coord_path = tool_dir.join(&spec.baml_file);
        if !coord_path.is_file() {
            bail!(
                "tool '{}': coordination file not found at {}",
                manifest.name,
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
                manifest.name,
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
    fn scaffolded_manifest_passes_runtime_validator() {
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
            if file.relative_path == "tool-manifest.json" {
                std::fs::write(tmp.path().join(file.relative_path), file.content.as_bytes())
                    .expect("write manifest");
                break;
            }
        }

        run(tmp.path().to_str().expect("utf8 path")).expect("validator should pass scaffold");
    }
}
