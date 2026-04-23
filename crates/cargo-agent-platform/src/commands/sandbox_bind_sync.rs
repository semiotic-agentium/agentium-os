//! `sandbox-bind-sync` subcommand implementation.
//!
//! Synchronises bind sandbox metadata (`runtime.image.path` + `runtime_digest`)
//! with a concrete rootfs directory. Optionally builds/exports rootfs from a
//! Docker image first.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::sandbox_runtime_digest_for_bind;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Serialize)]
struct SyncSummary {
    tool_dir: String,
    metadata_path: String,
    rootfs_path: String,
    runtime_digest: String,
    docker_mode: bool,
    checked: bool,
    dry_run: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SandboxBindSyncRunArgs<'a> {
    pub tool_dir: &'a str,
    pub rootfs: &'a str,
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
        bail!("--check cannot be combined with --dry-run (no metadata changes are written)");
    }

    let docker_mode = dockerfile.is_some() || image.is_some();
    if docker_mode && (dockerfile.is_none() || image.is_none()) {
        bail!("--dockerfile and --image must be provided together");
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

    let metadata_path = tool_dir.join("tool-metadata.json");
    if !metadata_path.exists() {
        bail!("missing tool metadata: {}", metadata_path.display());
    }

    let rootfs = resolve_path_from_tool_dir(&tool_dir, rootfs);

    if docker_mode {
        let dockerfile = dockerfile.expect("checked above");
        let image = image.expect("checked above");
        let dockerfile = resolve_path_from_tool_dir(&tool_dir, dockerfile);
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

    let digest = sandbox_runtime_digest_for_bind(&canonical_rootfs)?;

    if !dry_run {
        patch_metadata(&metadata_path, &canonical_rootfs, &digest)?;
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
        metadata_path: metadata_path.display().to_string(),
        rootfs_path: canonical_rootfs.display().to_string(),
        runtime_digest: digest,
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
            println!("Bind metadata patched.");
        }
        println!("  tool dir:       {}", summary.tool_dir);
        println!("  metadata:       {}", summary.metadata_path);
        println!("  bind path:      {}", summary.rootfs_path);
        println!("  runtime_digest: {}", summary.runtime_digest);
        if docker_mode {
            println!("  mode:           docker-assisted");
        } else {
            println!("  mode:           existing-rootfs");
        }
    }

    Ok(())
}

fn resolve_path_from_tool_dir(tool_dir: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() { p } else { tool_dir.join(p) }
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

fn patch_metadata(metadata_path: &Path, bind_path: &Path, digest: &str) -> Result<()> {
    let raw = fs::read_to_string(metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let mut metadata: Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {} as JSON", metadata_path.display()))?;

    let obj = metadata
        .as_object_mut()
        .ok_or_else(|| anyhow!("metadata root is not a JSON object"))?;

    let runtime = obj.entry("runtime").or_insert_with(|| json!({}));
    let runtime_obj = runtime
        .as_object_mut()
        .ok_or_else(|| anyhow!("metadata.runtime must be a JSON object"))?;

    if let Some(kind) = runtime_obj.get("kind").and_then(Value::as_str)
        && kind != "sandbox"
    {
        bail!("metadata.runtime.kind must be 'sandbox' for bind sync (got '{kind}')");
    }

    runtime_obj.insert("kind".to_string(), Value::String("sandbox".to_string()));
    runtime_obj.insert(
        "image".to_string(),
        json!({
            "kind": "bind",
            "path": bind_path.display().to_string(),
        }),
    );

    obj.insert(
        "runtime_digest".to_string(),
        Value::String(digest.to_string()),
    );

    let patched = serde_json::to_string_pretty(&metadata)? + "\n";
    fs::write(metadata_path, patched)
        .with_context(|| format!("failed to write {}", metadata_path.display()))?;

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
    use super::*;

    #[test]
    fn patch_metadata_sets_bind_image_and_digest() {
        let tmp = tempfile::tempdir().expect("tmp");
        let metadata_path = tmp.path().join("tool-metadata.json");
        std::fs::write(
            &metadata_path,
            r#"{
  "name": "support/echo",
  "runtime": {
    "kind": "sandbox",
    "image": {"kind":"bind","path":"<rootfs-path>"},
    "entrypoint": ["/tool-adapter"]
  },
  "runtime_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
}
"#,
        )
        .expect("write");

        let bind_path = tmp.path().join("rootfs");
        std::fs::create_dir_all(&bind_path).expect("rootfs");

        let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        patch_metadata(&metadata_path, &bind_path, digest).expect("patch");

        let raw = std::fs::read_to_string(&metadata_path).expect("read");
        let parsed: Value = serde_json::from_str(&raw).expect("json");

        assert_eq!(
            parsed
                .get("runtime")
                .and_then(|v| v.get("image"))
                .and_then(|v| v.get("kind"))
                .and_then(Value::as_str),
            Some("bind")
        );
        let bind_path_str = bind_path.display().to_string();
        assert_eq!(
            parsed
                .get("runtime")
                .and_then(|v| v.get("image"))
                .and_then(|v| v.get("path"))
                .and_then(Value::as_str),
            Some(bind_path_str.as_str())
        );
        assert_eq!(
            parsed.get("runtime_digest").and_then(Value::as_str),
            Some(digest)
        );
    }

    #[test]
    fn dry_run_with_check_is_rejected() {
        let err = run(SandboxBindSyncRunArgs {
            tool_dir: ".",
            rootfs: ".",
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
