// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Packager implementation for creating tar.gz agent packages

use std::{
    fs,
    path::{Path, PathBuf},
};

use baml_rt_core::AgentManifest;
use baml_rt_tools::external_tools::EXTERNAL_TOOLS_LOCKFILE_NAME;
use flate2::{Compression, write::GzEncoder};
use tar::{Builder, Header};
use uuid::Uuid;

use crate::builder::{
    error::{BamlBuilderError, Result},
    traits::Packager,
    types::{AgentDir, BuildDir},
};

/// Standard packager implementation.
///
/// Packaging is pure `std::fs` plus `tar`/`flate2`; the
/// [`FileSystem`](crate::builder::traits::FileSystem) trait abstraction is
/// not exercised here, so this is a unit struct.
#[derive(Default)]
pub struct StdPackager;

impl StdPackager {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Packager for StdPackager {
    async fn package(
        &self,
        agent_dir: &AgentDir,
        build_dir: &BuildDir,
        output: &Path,
    ) -> Result<()> {
        let agent_path = agent_dir.as_path().to_path_buf();
        let build_path = build_dir.as_path().to_path_buf();
        let output_path = output.to_path_buf();
        tokio::task::spawn_blocking(move || {
            package_blocking(&agent_path, &build_path, &output_path)
        })
        .await
        .map_err(|e| BamlBuilderError::BlockingTaskJoin { source: e })?
    }
}

/// Synchronous packaging body. Runs filesystem traversal, file reads, tar
/// construction, and gzip compression; intended to be invoked from a
/// [`tokio::task::spawn_blocking`] task so it does not stall executor threads.
fn package_blocking(agent_dir: &Path, build_dir: &Path, output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    let tar_gz = fs::File::create(output)?;
    let enc = GzEncoder::new(tar_gz, Compression::default());
    let mut tar = Builder::new(enc);

    // Add manifest.json (ensure signature exists; use dist/index.js as entry_point when built)
    let manifest_path = agent_dir.join("manifest.json");
    if manifest_path.exists() {
        let content = fs::read_to_string(&manifest_path)?;
        let mut manifest: AgentManifest =
            serde_json::from_str(&content).map_err(BamlBuilderError::Json)?;
        if manifest.signature.is_empty() {
            manifest.signature = Uuid::new_v4().to_string();
        }
        if build_dir.join("dist").join("index.js").exists() {
            manifest.entry_point = "dist/index.js".to_string();
        }
        let content = serde_json::to_string_pretty(&manifest).map_err(BamlBuilderError::Json)?;
        let mut header = Header::new_gnu();
        header
            .set_path("manifest.json")
            .map_err(BamlBuilderError::TarHeaderPath)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_bytes())?;
    }

    // Add package.json if it exists
    let package_json_path = agent_dir.join("package.json");
    if package_json_path.exists() {
        let content = fs::read_to_string(&package_json_path)?;
        let mut header = Header::new_gnu();
        header
            .set_path("package.json")
            .map_err(BamlBuilderError::TarHeaderPath)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_bytes())?;
    }

    // Add baml_src (required - runtime loads from this)
    let baml_src_build = build_dir.join("baml_src");
    if baml_src_build.exists() {
        add_directory_to_tar(&mut tar, &baml_src_build, "baml_src")?;
    }

    // Add src (entry_point is typically src/index.ts; runner evaluates this)
    let src_dir = agent_dir.join("src");
    if src_dir.exists() {
        add_directory_to_tar(&mut tar, &src_dir, "src")?;
    }

    // Add dist
    let dist_build = build_dir.join("dist");
    if dist_build.exists() {
        add_directory_to_tar(&mut tar, &dist_build, "dist")?;
    }

    // Add session_plan_functions.json (generated from BAML IR; runtime uses it to resolve tool from function name)
    let session_plan_manifest = build_dir.join("session_plan_functions.json");
    if session_plan_manifest.exists() {
        let content = fs::read_to_string(&session_plan_manifest)?;
        let mut header = Header::new_gnu();
        header
            .set_path("session_plan_functions.json")
            .map_err(BamlBuilderError::TarHeaderPath)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_bytes())?;
    }

    // Add MCP registry snapshots resolved during build, if any.
    let mcp_dir = build_dir.join("mcp");
    if mcp_dir.exists() {
        add_directory_to_tar(&mut tar, &mcp_dir, "mcp")?;
    }

    // Add external-tool registry snapshots resolved during build, if any.
    let external_tools_dir = build_dir.join("external-tools");
    if external_tools_dir.exists() {
        add_directory_to_tar(&mut tar, &external_tools_dir, "external-tools")?;
    }

    let unified_manifest = build_dir.join("unified_step_executor_functions.json");
    if unified_manifest.exists() {
        let content = fs::read_to_string(&unified_manifest)?;
        let mut header = Header::new_gnu();
        header
            .set_path("unified_step_executor_functions.json")
            .map_err(BamlBuilderError::TarHeaderPath)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_bytes())?;
    }

    // Add external_tools.lock.json (always written by type generation).
    let external_lockfile = build_dir.join(EXTERNAL_TOOLS_LOCKFILE_NAME);
    if external_lockfile.exists() {
        let content = fs::read_to_string(&external_lockfile)?;
        let mut header = Header::new_gnu();
        header
            .set_path(EXTERNAL_TOOLS_LOCKFILE_NAME)
            .map_err(BamlBuilderError::TarHeaderPath)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_bytes())?;
    }

    tar.finish()?;
    Ok(())
}

fn add_directory_to_tar(
    tar: &mut Builder<GzEncoder<fs::File>>,
    dir: &Path,
    prefix: &str,
) -> Result<()> {
    fn collect_all_files(
        dir: &Path,
        files: &mut Vec<PathBuf>,
    ) -> std::result::Result<(), std::io::Error> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                collect_all_files(&path, files)?;
            } else {
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    collect_all_files(dir, &mut files)?;

    for file_path in files {
        let content = fs::read_to_string(&file_path)?;
        let relative_path = file_path.strip_prefix(dir).map_err(|_| {
            BamlBuilderError::InvalidArgument(format!(
                "File {} is not under directory {}",
                file_path.display(),
                dir.display()
            ))
        })?;

        let tar_path = format!("{}/{}", prefix, relative_path.display());
        let mut header = Header::new_gnu();
        header
            .set_path(&tar_path)
            .map_err(BamlBuilderError::TarHeaderPath)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_bytes())?;
    }

    Ok(())
}
