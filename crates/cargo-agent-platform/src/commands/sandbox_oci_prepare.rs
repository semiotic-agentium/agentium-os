//! Materialize OCI sandbox sidecar bundle from `tool-metadata.json`.
//!
//! This command keeps OCI image preparation on the same shared bundle helper
//! path as bind sync (`render_sidecar_bundle`), so sidecar contract generation
//! does not drift across sandbox image sources.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::{
    SIDECAR_BUNDLE_REL_PATH, SandboxImageRef, ToolRuntime, read_runtime_external_metadata,
    render_sidecar_bundle,
};
use console::style;
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct SandboxOciPrepareRunArgs<'a> {
    pub tool_dir: &'a str,
    pub output: Option<&'a str>,
    pub check: bool,
    pub dry_run: bool,
    pub as_json: bool,
}

#[derive(Debug, Serialize)]
struct Summary {
    tool_dir: String,
    metadata: String,
    output: String,
    tool_name: String,
    image_ref: String,
    dry_run: bool,
    check: bool,
}

pub fn run(args: SandboxOciPrepareRunArgs<'_>) -> Result<()> {
    let tool_dir = Path::new(args.tool_dir);
    if !tool_dir.exists() {
        bail!("tool directory does not exist: {}", tool_dir.display());
    }
    let tool_dir = fs::canonicalize(tool_dir)
        .with_context(|| format!("failed to canonicalize tool dir {}", tool_dir.display()))?;

    let metadata_path = tool_dir.join("tool-metadata.json");
    if !metadata_path.exists() {
        bail!("missing tool-metadata.json at {}", metadata_path.display());
    }

    let metadata = read_runtime_external_metadata(&tool_dir)?;

    let runtime = metadata.runtime.as_ref().ok_or_else(|| {
        anyhow!(
            "sidecar bundle generation requires runtime.kind=sandbox (tool: {})",
            metadata.name
        )
    })?;

    let image_ref = match runtime {
        ToolRuntime::Sandbox(spec) => match &spec.image {
            SandboxImageRef::Oci { r#ref } => r#ref,
            other => {
                bail!(
                    "sandbox-oci-prepare requires runtime.image.kind=oci (tool: {}, got: {:?})",
                    metadata.name,
                    other
                );
            }
        },
        _ => {
            bail!(
                "sidecar bundle generation requires runtime.kind=sandbox (tool: {})",
                metadata.name
            );
        }
    };

    let Some((_, digest_from_image)) = image_ref.split_once('@') else {
        bail!("sandbox oci image must be digest-pinned (missing @sha256:): {image_ref}");
    };
    if !digest_from_image.starts_with("sha256:") {
        bail!("sandbox oci image must include @sha256:<64-hex>: {image_ref}");
    }

    let bundle = render_sidecar_bundle(&metadata)
        .map_err(|e| anyhow!("failed to render sidecar bundle: {e}"))?;

    let output_path = resolve_output_path(&tool_dir, args.output);

    if !args.dry_run {
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create sidecar parent directory {}",
                    parent.display()
                )
            })?;
        }
        let json = serde_json::to_string_pretty(&bundle)
            .with_context(|| format!("failed to serialize sidecar bundle for {}", metadata.name))?;
        fs::write(&output_path, format!("{json}\n")).with_context(|| {
            format!(
                "failed to write sidecar bundle to {}",
                output_path.display()
            )
        })?;
    }

    if args.check {
        crate::commands::check_external_tool::run(tool_dir.to_string_lossy().as_ref())?;
    }

    let summary = Summary {
        tool_dir: tool_dir.display().to_string(),
        metadata: metadata_path.display().to_string(),
        output: output_path.display().to_string(),
        tool_name: metadata.name.clone(),
        image_ref: image_ref.to_string(),
        dry_run: args.dry_run,
        check: args.check,
    };

    if args.as_json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "{} OCI sidecar bundle {}",
            style("✓").green(),
            if args.dry_run { "planned" } else { "written" }
        );
        println!("  tool:           {}", summary.tool_name);
        println!("  metadata:       {}", summary.metadata);
        println!("  output:         {}", summary.output);
        println!("  image_ref:      {}", summary.image_ref);
        if args.check {
            println!("  validation:     check-external-tool passed");
        }
    }

    Ok(())
}

fn resolve_output_path(tool_dir: &Path, output: Option<&str>) -> PathBuf {
    if let Some(raw) = output {
        let p = Path::new(raw);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        return tool_dir.join(p);
    }

    tool_dir
        .join("adapter/sidecars")
        .join(SIDECAR_BUNDLE_REL_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_metadata(path: &Path, image_kind: &str) {
        let image_json = if image_kind == "oci" {
            r#"{"kind":"oci","ref":"ghcr.io/acme/echo@sha256:1111111111111111111111111111111111111111111111111111111111111111"}"#
        } else {
            r#"{"kind":"bind","path":"./rootfs"}"#
        };

        let json = format!(
            r#"{{
  "tool_abi_version": "1",
  "name": "support/echo",
  "description": "echo",
  "bundle": "support",
  "local_name": "echo",
  "access_level": "read",
  "tags": [],
  "invocation_mode": "single_shot",
  "schemas": {{
    "input": {{"type": "object"}},
    "output": {{"type": "object"}}
  }},
  "secrets": [],
  "capabilities": {{}},
  "runtime": {{
    "kind": "sandbox",
    "image": {image_json},
    "entrypoint": ["/tool-adapter"],
    "adapter": {{
      "schema_version": 1,
      "protocol": "jsonrpc-stdio",
      "command": ["python3", "/opt/tool/main.py"],
      "workdir": "/opt/tool"
    }}
  }}
}}"#
        );

        fs::write(path, json).expect("write metadata");
    }

    #[test]
    #[ignore = "sandbox-lane-only"]
    fn run_writes_default_oci_sidecar_bundle() {
        let tmp = tempfile::tempdir().expect("tmp");
        let metadata_path = tmp.path().join("tool-metadata.json");
        write_metadata(&metadata_path, "oci");

        run(SandboxOciPrepareRunArgs {
            tool_dir: tmp.path().to_str().expect("utf8"),
            output: None,
            check: false,
            dry_run: false,
            as_json: false,
        })
        .expect("oci prepare should succeed");

        let out = tmp
            .path()
            .join("adapter/sidecars")
            .join(SIDECAR_BUNDLE_REL_PATH);
        assert!(out.exists(), "sidecar bundle should be written");

        let raw = fs::read_to_string(out).expect("read sidecar");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("parse sidecar");
        assert!(
            v["manifest"]["supported_methods"]
                .as_array()
                .expect("methods array")
                .iter()
                .any(|m| m.as_str() == Some("tool/schema"))
        );
    }

    #[test]
    #[ignore = "sandbox-lane-only"]
    fn run_rejects_non_oci_sandbox_image() {
        let tmp = tempfile::tempdir().expect("tmp");
        let metadata_path = tmp.path().join("tool-metadata.json");
        write_metadata(&metadata_path, "bind");

        let err = run(SandboxOciPrepareRunArgs {
            tool_dir: tmp.path().to_str().expect("utf8"),
            output: None,
            check: false,
            dry_run: false,
            as_json: false,
        })
        .expect_err("bind image should fail oci prepare");

        assert!(
            err.to_string().contains("runtime.image.kind=oci"),
            "unexpected error: {err}"
        );
    }
}
