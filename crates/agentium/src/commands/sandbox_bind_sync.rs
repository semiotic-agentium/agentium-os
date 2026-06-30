// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `sandbox-bind-sync` subcommand implementation.
//!
//! Materializes host-resolved bind state next to a hand-written
//! `tool-manifest.json`:
//! - writes a sibling `tool-manifest.lock.json` carrying the canonical
//!   absolute rootfs path;
//! - writes the in-rootfs sidecar bundle at `etc/agentium/tool-bundle.json`.
//!
//! The committed source `tool-manifest.json` is **never** mutated, so example
//! tools stay portable across contributors. Optionally builds/exports the
//! rootfs from a Docker image first.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::{
    MetadataSchemas, SIDECAR_BUNDLE_REL_PATH, SandboxImageRef, ToolRuntime, ToolRuntimeLock,
    read_external_manifest, render_sidecar_bundle,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct SyncSummary {
    tool_dir: String,
    manifest_path: String,
    rootfs_path: String,
    docker_mode: bool,
    checked: bool,
    dry_run: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SandboxBindSyncRunArgs<'a> {
    pub tool_dir: &'a str,
    pub rootfs: Option<&'a str>,
    pub dockerfile: Option<&'a str>,
    pub image: Option<&'a str>,
    pub force: bool,
    pub check: bool,
    pub dry_run: bool,
    pub as_json: bool,
}

pub fn run(args: SandboxBindSyncRunArgs<'_>) -> Result<()> {
    let SandboxBindSyncRunArgs {
        tool_dir,
        rootfs,
        dockerfile,
        image,
        force,
        check,
        dry_run,
        as_json,
    } = args;

    if dry_run && check {
        bail!("--check cannot be combined with --dry-run (no manifest changes are written)");
    }

    if dockerfile.is_some() && image.is_none() {
        bail!("--dockerfile requires --image");
    }

    let tool_dir = Path::new(tool_dir);
    if !tool_dir.exists() {
        bail!("tool directory does not exist: {}", tool_dir.display());
    }
    if !tool_dir.is_dir() {
        bail!("tool path is not a directory: {}", tool_dir.display());
    }

    let tool_dir = fs::canonicalize(tool_dir)
        .with_context(|| format!("failed to canonicalize tool dir: {}", tool_dir.display()))?;

    let manifest_path = tool_dir.join("tool-manifest.json");
    if !manifest_path.exists() {
        bail!("missing tool manifest: {}", manifest_path.display());
    }

    // Validate source portability up-front so `--dry-run` catches the same
    // pollution a real run would reject. The returned manifest path also lets
    // --rootfs default to the authored bind path.
    let source_rootfs = validate_bind_source(&tool_dir)?;
    let source_rootfs_resolved = resolve_tool_relative_path(&tool_dir, &source_rootfs);
    let rootfs = match rootfs {
        Some(raw) => {
            let resolved = resolve_path_from_tool_dir(&tool_dir, raw);
            if lexical_normalize(&resolved) != lexical_normalize(&source_rootfs_resolved) {
                eprintln!(
                    "warning: --rootfs ({}) differs from source manifest runtime.image.path ({}); tool-manifest.lock.json will use --rootfs",
                    resolved.display(),
                    source_rootfs_resolved.display()
                );
            }
            resolved
        }
        None => source_rootfs_resolved,
    };

    let docker_mode = image.is_some();
    if let Some(image) = image {
        let dockerfile = dockerfile
            .map(|raw| resolve_path_from_tool_dir(&tool_dir, raw))
            .unwrap_or_else(|| tool_dir.join("adapter/Dockerfile"));
        if !dockerfile.is_file() {
            bail!(
                "Docker-assisted bind sync requires a Dockerfile at {}. Pass --dockerfile to override the default adapter/Dockerfile.",
                dockerfile.display()
            );
        }
        build_and_export_rootfs(&tool_dir, &dockerfile, image, &rootfs, force, dry_run)?;
    } else {
        validate_existing_rootfs(&rootfs)?;
    }

    let canonical_rootfs = fs::canonicalize(&rootfs)
        .with_context(|| format!("bind path does not resolve: {}", rootfs.display()))?;
    if !canonical_rootfs.is_dir() {
        bail!(
            "bind path is not a directory: {}",
            canonical_rootfs.display()
        );
    }

    if !dry_run {
        write_runtime_lock(&tool_dir, &canonical_rootfs)?;
        write_runtime_sidecars(&manifest_path, &canonical_rootfs)?;
    }

    if check {
        crate::commands::check_external_tool::run(
            tool_dir
                .to_str()
                .ok_or_else(|| anyhow!("tool dir is not valid UTF-8"))?,
        )?;
    }

    let summary = SyncSummary {
        tool_dir: tool_dir.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        rootfs_path: canonical_rootfs.display().to_string(),
        docker_mode,
        checked: check,
        dry_run,
    };

    if as_json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        if dry_run {
            println!("Dry run successful (no files changed).");
        } else {
            println!("Bind runtime lock written (tool-manifest.lock.json).");
        }
        println!("  tool dir:       {}", summary.tool_dir);
        println!("  manifest:       {}", summary.manifest_path);
        println!("  bind path:      {}", summary.rootfs_path);
        if docker_mode {
            println!("  mode:           docker-assisted");
        } else {
            println!("  mode:           existing-rootfs");
        }
    }

    Ok(())
}

fn resolve_path_from_tool_dir(tool_dir: &Path, raw: &str) -> PathBuf {
    resolve_tool_relative_path(tool_dir, &PathBuf::from(raw))
}

fn resolve_tool_relative_path(tool_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        tool_dir.join(path)
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push(component.as_os_str());
                }
            }
            _ => out.push(component.as_os_str()),
        }
    }
    out
}

fn validate_existing_rootfs(rootfs: &Path) -> Result<()> {
    if !rootfs.exists() {
        bail!("bind rootfs path does not exist: {}", rootfs.display());
    }
    if !rootfs.is_dir() {
        bail!("bind rootfs path is not a directory: {}", rootfs.display());
    }
    Ok(())
}

fn build_and_export_rootfs(
    tool_dir: &Path,
    dockerfile: &Path,
    image: &str,
    rootfs: &Path,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    if !dockerfile.exists() {
        bail!("dockerfile does not exist: {}", dockerfile.display());
    }

    if rootfs.exists() {
        if force {
            if !dry_run {
                fs::remove_dir_all(rootfs)
                    .with_context(|| format!("failed to remove {}", rootfs.display()))?;
            }
        } else if rootfs.read_dir()?.next().is_some() {
            bail!(
                "rootfs directory already exists and is non-empty: {}\nHint: pass --force to recreate it.",
                rootfs.display()
            );
        }
    }

    if dry_run {
        return Ok(());
    }

    if let Some(parent) = rootfs.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(rootfs)?;

    run_command(
        Command::new("docker")
            .arg("build")
            .arg("-t")
            .arg(image)
            .arg("-f")
            .arg(dockerfile)
            .arg(tool_dir),
        "docker build",
    )?;

    let cid = command_output(
        Command::new("docker").arg("create").arg(image),
        "docker create",
    )?
    .trim()
    .to_string();

    let tar_file = tempfile::NamedTempFile::new().context("failed to allocate temp tar file")?;
    let tar_path = tar_file.path().to_path_buf();

    let export_result = run_command(
        Command::new("docker")
            .arg("export")
            .arg("-o")
            .arg(&tar_path)
            .arg(&cid),
        "docker export",
    );

    let _ = run_command(Command::new("docker").arg("rm").arg(&cid), "docker rm");

    export_result?;

    run_command(
        Command::new("tar")
            .arg("-xf")
            .arg(&tar_path)
            .arg("-C")
            .arg(rootfs),
        "tar extract",
    )?;

    Ok(())
}

/// Validate that the source `tool-manifest.json` declares a portable bind
/// sandbox runtime: kind=sandbox, image.kind=bind, relative path. Run before
/// any writes so `--dry-run` catches the same
/// errors a real sync would, and return the authored bind path so callers can
/// default `--rootfs` to it.
fn validate_bind_source(tool_dir: &Path) -> Result<PathBuf> {
    let source = read_external_manifest(tool_dir)
        .with_context(|| format!("failed to read source manifest in {}", tool_dir.display()))?;

    let source_rootfs = match source.runtime.as_ref() {
        Some(ToolRuntime::Sandbox(spec)) => match &spec.image {
            SandboxImageRef::Bind { path } => {
                if path.is_absolute() {
                    bail!(
                        "source tool-manifest.json declares an absolute bind path ({}); \
                         use a relative path like \"./rootfs\" — host-resolved paths belong \
                         in tool-manifest.lock.json",
                        path.display()
                    );
                }
                path.clone()
            }
            other => bail!(
                "sandbox-bind-sync requires runtime.image.kind = 'bind' in source manifest (got {other:?})"
            ),
        },
        Some(other) => bail!(
            "sandbox-bind-sync requires runtime.kind = 'sandbox' in source manifest (got '{}')",
            match other {
                ToolRuntime::Process(_) => "process",
                ToolRuntime::Sandbox(_) => unreachable!(),
            }
        ),
        None => bail!(
            "source tool-manifest.json has no runtime declaration; declare a sandbox/bind runtime first"
        ),
    };

    Ok(source_rootfs)
}

/// Write the per-tool runtime lock sidecar (`tool-manifest.lock.json`) next
/// to the source `tool-manifest.json`. Pure writer — callers must invoke
/// [`validate_bind_source`] first.
fn write_runtime_lock(tool_dir: &Path, bind_path: &Path) -> Result<()> {
    let lock = ToolRuntimeLock::new_bind(bind_path.to_path_buf());
    lock.write_to_dir(tool_dir).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

fn write_runtime_sidecars(manifest_path: &Path, rootfs: &Path) -> Result<()> {
    let tool_dir = manifest_path
        .parent()
        .ok_or_else(|| anyhow!("manifest path has no parent: {}", manifest_path.display()))?;
    let manifest = read_external_manifest(tool_dir)?;
    let metadata = manifest.into_metadata(MetadataSchemas {
        input: serde_json::json!({"type": "object"}),
        output: serde_json::json!({"type": "object"}),
        events: Vec::new(),
    });
    let bundle = render_sidecar_bundle(&metadata)
        .map_err(|e| anyhow!("failed to render sidecar bundle: {e}"))?;

    let bundle_path = rootfs.join(SIDECAR_BUNDLE_REL_PATH);
    if let Some(parent) = bundle_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::write(&bundle_path, serde_json::to_string_pretty(&bundle)? + "\n")
        .with_context(|| format!("failed to write {}", bundle_path.display()))?;

    Ok(())
}

fn run_command(cmd: &mut Command, label: &str) -> Result<()> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to execute {label}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "{label} failed (status: {}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout.trim(),
        stderr.trim()
    );
}

fn command_output(cmd: &mut Command, label: &str) -> Result<String> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to execute {label}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "{label} failed (status: {}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout.trim(),
        stderr.trim()
    );
}

#[cfg(test)]
mod tests {
    use baml_rt_tools::external_tools::{RUNTIME_LOCK_FILE_NAME, read_runtime_lock};
    use serde_json::Value;

    use super::*;

    const PORTABLE_SOURCE: &str = r#"{
  "tool_abi_version": "1",
  "name": "dev/echo",
  "description": "echo",
  "bundle": "dev",
  "local_name": "echo",
  "access_level": "read",
  "tags": [],
  "invocation_mode": "single_shot",
  "session_policy": "strict",
  "secrets": [],
  "capabilities": {},
  "runtime": {
    "kind": "sandbox",
    "image": {"kind":"bind","path":"./rootfs"},
    "entrypoint": ["/tool-adapter"],
    "adapter": {
      "schema_version": 1,
      "protocol": "jsonrpc-stdio",
      "command": ["/tool-adapter"],
      "workdir": "/"
    }
  }
}
"#;

    #[test]
    fn write_runtime_lock_writes_sidecar_and_leaves_source_intact() {
        let tmp = tempfile::tempdir().expect("tmp");
        let manifest_path = tmp.path().join("tool-manifest.json");
        std::fs::write(&manifest_path, PORTABLE_SOURCE).expect("write source");

        let bind_path = tmp.path().join("rootfs");
        std::fs::create_dir_all(&bind_path).expect("rootfs");

        write_runtime_lock(tmp.path(), &bind_path).expect("write lock");

        // Source file untouched.
        let source_after = std::fs::read_to_string(&manifest_path).expect("read source");
        assert_eq!(source_after, PORTABLE_SOURCE);

        // Lock sidecar present and carries our values.
        let lock_path = tmp.path().join(RUNTIME_LOCK_FILE_NAME);
        assert!(lock_path.exists(), "lock sidecar must exist");
        let lock = read_runtime_lock(tmp.path())
            .expect("read lock")
            .expect("lock present");
        assert_eq!(lock.image_path_abs.as_deref(), Some(bind_path.as_path()));
    }

    #[test]
    fn validate_bind_source_rejects_absolute_path() {
        let tmp = tempfile::tempdir().expect("tmp");
        let polluted =
            PORTABLE_SOURCE.replace(r#""path":"./rootfs""#, r#""path":"/abs/leaked/path""#);
        std::fs::write(tmp.path().join("tool-manifest.json"), polluted).expect("write");

        let err = validate_bind_source(tmp.path()).expect_err("must reject");
        assert!(
            err.to_string().contains("absolute bind path"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_bind_source_accepts_portable_source() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join("tool-manifest.json"), PORTABLE_SOURCE).expect("write");
        validate_bind_source(tmp.path()).expect("portable source must validate");
    }

    #[test]
    fn rootfs_defaults_to_source_manifest_path() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join("tool-manifest.json"), PORTABLE_SOURCE).expect("write");
        std::fs::create_dir_all(tmp.path().join("rootfs")).expect("rootfs");

        run(SandboxBindSyncRunArgs {
            tool_dir: tmp.path().to_str().expect("utf8"),
            rootfs: None,
            dockerfile: None,
            image: None,
            force: false,
            check: false,
            dry_run: true,
            as_json: false,
        })
        .expect("dry-run should use metadata runtime.image.path as rootfs default");
    }

    #[test]
    fn dockerfile_without_image_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join("tool-manifest.json"), PORTABLE_SOURCE).expect("write");

        let err = run(SandboxBindSyncRunArgs {
            tool_dir: tmp.path().to_str().expect("utf8"),
            rootfs: None,
            dockerfile: Some("adapter/Dockerfile"),
            image: None,
            force: false,
            check: false,
            dry_run: false,
            as_json: false,
        })
        .expect_err("must reject");

        assert!(err.to_string().contains("--dockerfile requires --image"));
    }

    #[test]
    fn image_defaults_to_adapter_dockerfile_and_errors_when_missing() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join("tool-manifest.json"), PORTABLE_SOURCE).expect("write");

        let err = run(SandboxBindSyncRunArgs {
            tool_dir: tmp.path().to_str().expect("utf8"),
            rootfs: None,
            dockerfile: None,
            image: Some("dev-echo-sandbox:local"),
            force: false,
            check: false,
            dry_run: true,
            as_json: false,
        })
        .expect_err("missing default Dockerfile should fail before invoking docker");

        assert!(err.to_string().contains("adapter/Dockerfile"), "got: {err}");
    }

    #[test]
    #[ignore = "sandbox-lane-only"]
    fn write_runtime_sidecars_materializes_expected_files() {
        let tmp = tempfile::tempdir().expect("tmp");
        let tool_dir = tmp.path().join("tool");
        std::fs::create_dir_all(&tool_dir).expect("tool dir");
        let manifest_path = tool_dir.join("tool-manifest.json");
        std::fs::write(
            &manifest_path,
            r#"{
  "tool_abi_version": "1",
  "name": "dev/meteo-tool",
  "description": "",
  "bundle": "dev",
  "local_name": "meteo-tool",
  "access_level": "read",
  "tags": [],
  "invocation_mode": "single_shot",
  "session_policy": "strict",
  "secrets": [],
  "capabilities": {},
  "runtime": {
    "kind": "sandbox",
    "image": {"kind":"bind","path":"./rootfs"},
    "entrypoint": ["/tool-adapter"],
    "adapter": {
      "schema_version": 1,
      "protocol": "jsonrpc-stdio",
      "command": ["python3", "/opt/tool/main.py"],
      "workdir": "/opt/tool"
    }
  }
}
"#,
        )
        .expect("write metadata");

        let rootfs = tmp.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).expect("rootfs");

        write_runtime_sidecars(&manifest_path, &rootfs).expect("write sidecars");

        let bundle_path = rootfs.join(SIDECAR_BUNDLE_REL_PATH);
        assert!(bundle_path.exists(), "sidecar bundle must exist");

        let bundle_json: Value =
            serde_json::from_str(&std::fs::read_to_string(&bundle_path).expect("read bundle"))
                .expect("parse bundle");

        let runtime_json = bundle_json.get("runtime").expect("runtime section");
        assert_eq!(
            runtime_json.get("tool_id").and_then(Value::as_str),
            Some("dev/meteo-tool")
        );
        assert_eq!(
            runtime_json.get("protocol").and_then(Value::as_str),
            Some("jsonrpc-stdio")
        );
        assert_eq!(
            runtime_json.get("schema_version").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            runtime_json.get("workdir").and_then(Value::as_str),
            Some("/opt/tool")
        );
        let cmd = runtime_json
            .get("command")
            .and_then(Value::as_array)
            .expect("command array");
        assert_eq!(
            cmd.iter().filter_map(Value::as_str).collect::<Vec<_>>(),
            vec!["python3", "/opt/tool/main.py"]
        );

        let manifest_json = bundle_json.get("manifest").expect("manifest section");
        assert_eq!(
            manifest_json.get("tool_name").and_then(Value::as_str),
            Some("dev/meteo-tool")
        );
        let methods = manifest_json
            .get("supported_methods")
            .and_then(Value::as_array)
            .expect("methods");
        let method_strs: Vec<&str> = methods.iter().filter_map(Value::as_str).collect();
        assert!(method_strs.contains(&"tool/describe"));
        assert!(method_strs.contains(&"tool/schema"));
        assert!(method_strs.contains(&"tool/invoke"));
    }

    #[test]
    fn dry_run_with_check_is_rejected() {
        let err = run(SandboxBindSyncRunArgs {
            tool_dir: ".",
            rootfs: Some("."),
            dockerfile: None,
            image: None,
            force: false,
            check: true,
            dry_run: true,
            as_json: false,
        })
        .expect_err("must reject");

        assert!(
            err.to_string()
                .contains("--check cannot be combined with --dry-run")
        );
    }
}
