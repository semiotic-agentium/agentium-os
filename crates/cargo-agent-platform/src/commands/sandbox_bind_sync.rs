//! `sandbox-bind-sync` subcommand implementation.
//!
//! Materializes host-resolved bind state next to a hand-written
//! `tool-metadata.json`:
//! - writes a sibling `tool-metadata.lock.json` carrying the canonical
//!   absolute rootfs path and computed `runtime_digest`;
//! - writes the in-rootfs sidecar bundle at `etc/agentium/tool-bundle.json`.
//!
//! The committed source `tool-metadata.json` is **never** mutated, so example
//! tools stay portable across contributors. Optionally builds/exports the
//! rootfs from a Docker image first.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::{
    SIDECAR_BUNDLE_REL_PATH, ToolRuntime, ToolRuntimeLock, read_external_metadata,
    read_runtime_external_metadata, read_sidecar_bundle, render_sidecar_bundle,
    sandbox_runtime_digest_for_bind, verify_runtime_digest,
};
use serde::Serialize;

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

    // Validate source portability up-front so `--dry-run` catches the same
    // pollution a real run would reject. The writer no longer re-validates.
    validate_bind_source(&tool_dir)?;

    let digest = sandbox_runtime_digest_for_bind(&canonical_rootfs)?;

    if !dry_run {
        write_runtime_lock(&tool_dir, &canonical_rootfs, &digest)?;
        write_runtime_sidecars(&metadata_path, &canonical_rootfs, &digest)?;
        verify_runtime_sidecar_digest(&canonical_rootfs, &digest)?;
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
            println!("Bind runtime lock written (tool-metadata.lock.json).");
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

/// Validate that the source `tool-metadata.json` declares a portable bind
/// sandbox runtime: kind=sandbox, image.kind=bind, relative path, no
/// `runtime_digest`. Run before any writes so `--dry-run` catches the same
/// errors a real sync would.
fn validate_bind_source(tool_dir: &Path) -> Result<()> {
    let source = read_external_metadata(tool_dir)
        .with_context(|| format!("failed to read source metadata in {}", tool_dir.display()))?;

    match source.runtime.as_ref() {
        Some(ToolRuntime::Sandbox(spec)) => match &spec.image {
            baml_rt_tools::external_tools::SandboxImageRef::Bind { path } => {
                if path.is_absolute() {
                    bail!(
                        "source tool-metadata.json declares an absolute bind path ({}); \
                         use a relative path like \"./rootfs\" — host-resolved paths belong \
                         in tool-metadata.lock.json",
                        path.display()
                    );
                }
            }
            other => bail!(
                "sandbox-bind-sync requires runtime.image.kind = 'bind' in source metadata (got {other:?})"
            ),
        },
        Some(other) => bail!(
            "sandbox-bind-sync requires runtime.kind = 'sandbox' in source metadata (got '{}')",
            match other {
                ToolRuntime::Process(_) => "process",
                ToolRuntime::Sandbox(_) => unreachable!(),
            }
        ),
        None => bail!(
            "source tool-metadata.json has no runtime declaration; declare a sandbox/bind runtime first"
        ),
    }

    if source.runtime_digest.is_some() {
        bail!(
            "source tool-metadata.json contains 'runtime_digest'; remove it — \
             the digest belongs in tool-metadata.lock.json"
        );
    }

    Ok(())
}

/// Write the per-tool runtime lock sidecar (`tool-metadata.lock.json`) next
/// to the source `tool-metadata.json`. Pure writer — callers must invoke
/// [`validate_bind_source`] first.
fn write_runtime_lock(tool_dir: &Path, bind_path: &Path, digest: &str) -> Result<()> {
    let lock = ToolRuntimeLock::new_bind(bind_path.to_path_buf(), digest.to_string());
    lock.write_to_dir(tool_dir).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

fn write_runtime_sidecars(metadata_path: &Path, rootfs: &Path, runtime_digest: &str) -> Result<()> {
    let tool_dir = metadata_path
        .parent()
        .ok_or_else(|| anyhow!("metadata path has no parent: {}", metadata_path.display()))?;
    let metadata = read_runtime_external_metadata(tool_dir)?;
    let bundle = render_sidecar_bundle(&metadata, runtime_digest)
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

fn verify_runtime_sidecar_digest(rootfs: &Path, expected_digest: &str) -> Result<()> {
    let bundle_path = rootfs.join(SIDECAR_BUNDLE_REL_PATH);
    let bundle = read_sidecar_bundle(&bundle_path).map_err(|e| {
        anyhow!(
            "failed to read sidecar bundle {}: {e}",
            bundle_path.display()
        )
    })?;
    verify_runtime_digest(&bundle, expected_digest).map_err(|e| {
        anyhow!(
            "runtime sidecar digest mismatch in {}: {e}",
            bundle_path.display()
        )
    })
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
  "schemas": {"input": {"type":"object"}, "output": {"type":"object"}},
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
        let metadata_path = tmp.path().join("tool-metadata.json");
        std::fs::write(&metadata_path, PORTABLE_SOURCE).expect("write source");

        let bind_path = tmp.path().join("rootfs");
        std::fs::create_dir_all(&bind_path).expect("rootfs");

        let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        write_runtime_lock(tmp.path(), &bind_path, digest).expect("write lock");

        // Source file untouched.
        let source_after = std::fs::read_to_string(&metadata_path).expect("read source");
        assert_eq!(source_after, PORTABLE_SOURCE);

        // Lock sidecar present and carries our values.
        let lock_path = tmp.path().join(RUNTIME_LOCK_FILE_NAME);
        assert!(lock_path.exists(), "lock sidecar must exist");
        let lock = read_runtime_lock(tmp.path())
            .expect("read lock")
            .expect("lock present");
        assert_eq!(lock.runtime_digest.as_deref(), Some(digest));
        assert_eq!(lock.image_path_abs.as_deref(), Some(bind_path.as_path()));
    }

    #[test]
    fn validate_bind_source_rejects_absolute_path() {
        let tmp = tempfile::tempdir().expect("tmp");
        let polluted =
            PORTABLE_SOURCE.replace(r#""path":"./rootfs""#, r#""path":"/abs/leaked/path""#);
        std::fs::write(tmp.path().join("tool-metadata.json"), polluted).expect("write");

        let err = validate_bind_source(tmp.path()).expect_err("must reject");
        assert!(
            err.to_string().contains("absolute bind path"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_bind_source_rejects_runtime_digest() {
        let tmp = tempfile::tempdir().expect("tmp");
        let polluted = PORTABLE_SOURCE.replace(
            r#""capabilities": {},"#,
            r#""capabilities": {}, "runtime_digest": "sha256:dead", "#,
        );
        std::fs::write(tmp.path().join("tool-metadata.json"), polluted).expect("write");

        let err = validate_bind_source(tmp.path()).expect_err("must reject");
        assert!(err.to_string().contains("runtime_digest"), "got: {err}");
    }

    #[test]
    fn validate_bind_source_accepts_portable_source() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join("tool-metadata.json"), PORTABLE_SOURCE).expect("write");
        validate_bind_source(tmp.path()).expect("portable source must validate");
    }

    #[test]
    #[ignore = "sandbox-lane-only"]
    fn write_runtime_sidecars_materializes_expected_files() {
        let tmp = tempfile::tempdir().expect("tmp");
        let tool_dir = tmp.path().join("tool");
        std::fs::create_dir_all(&tool_dir).expect("tool dir");
        let metadata_path = tool_dir.join("tool-metadata.json");
        std::fs::write(
            &metadata_path,
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
  "schemas": {"input": {"type":"object"}, "output": {"type":"object"}},
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
  },
  "runtime_digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222"
}
"#,
        )
        .expect("write metadata");

        let rootfs = tmp.path().join("rootfs");
        std::fs::create_dir_all(&rootfs).expect("rootfs");

        let digest = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        write_runtime_sidecars(&metadata_path, &rootfs, digest).expect("write sidecars");

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
            runtime_json.get("runtime_digest").and_then(Value::as_str),
            Some(digest)
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

        let schema_json = bundle_json.get("schema").expect("schema section");
        assert_eq!(
            schema_json.get("tool_name").and_then(Value::as_str),
            Some("dev/meteo-tool")
        );
        assert_eq!(
            schema_json.get("content_type").and_then(Value::as_str),
            Some("application/schema+json")
        );
        assert!(
            schema_json
                .get("content_digest")
                .and_then(Value::as_str)
                .is_some(),
            "schema.content_digest must exist"
        );

        verify_runtime_sidecar_digest(&rootfs, digest).expect("digest verify");
    }

    #[test]
    fn verify_runtime_sidecar_digest_detects_mismatch() {
        let tmp = tempfile::tempdir().expect("tmp");
        let rootfs = tmp.path();
        let dir = rootfs.join("etc/agentium");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("tool-bundle.json"),
            r#"{"runtime":{"schema_version":1,"tool_id":"dev/meteo-tool","runtime_digest":"sha256:aaaa","command":["python3","/opt/tool/main.py"],"protocol":"jsonrpc-stdio"},"manifest":{"tool_name":"dev/meteo-tool","protocol_version":"2","supported_methods":["tool/describe","tool/schema","tool/invoke"]},"schema":{"schema_version":1,"tool_name":"dev/meteo-tool","content_type":"application/schema+json","content_digest":"sha256:cccc","input":{"type":"object"},"output":{"type":"object"}}}"#,
        )
        .expect("sidecar");

        let err =
            verify_runtime_sidecar_digest(rootfs, "sha256:bbbb").expect_err("expected mismatch");
        let msg = err.to_string();
        assert!(msg.contains("digest mismatch"), "got: {msg}");
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
