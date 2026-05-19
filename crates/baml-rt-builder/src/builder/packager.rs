//! Packager implementation for creating tar.gz agent packages

use std::{fs, path::Path};

use baml_rt_core::AgentManifest;
use baml_rt_tools::external_tools::EXTERNAL_TOOLS_LOCKFILE_NAME;
use flate2::{Compression, write::GzEncoder};
use tar::{Builder, Header};
use uuid::Uuid;

use crate::builder::{
    error::{BamlBuilderError, Result},
    traits::{FileSystem, Packager},
    types::{AgentDir, BuildDir},
};

/// Standard packager implementation
pub struct StdPackager<FS> {
    filesystem: FS,
}

impl<FS: FileSystem> StdPackager<FS> {
    pub fn new(filesystem: FS) -> Self {
        Self { filesystem }
    }
}

#[async_trait::async_trait]
impl<FS: FileSystem> Packager for StdPackager<FS> {
    async fn package(
        &self,
        agent_dir: &AgentDir,
        build_dir: &BuildDir,
        output: &Path,
    ) -> Result<()> {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }

        let tar_gz = fs::File::create(output)?;
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = Builder::new(enc);

        // Add manifest.json (ensure signature exists; use dist/index.js as entry_point when built)
        let manifest_path = agent_dir.as_path().join("manifest.json");
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
            let content =
                serde_json::to_string_pretty(&manifest).map_err(BamlBuilderError::Json)?;
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
        let package_json_path = agent_dir.as_path().join("package.json");
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
            add_directory_to_tar(&mut tar, &baml_src_build, "baml_src", &self.filesystem)?;
        }

        // Add src (entry_point is typically src/index.ts; runner evaluates this)
        let src_dir = agent_dir.as_path().join("src");
        if src_dir.exists() {
            add_directory_to_tar(&mut tar, &src_dir, "src", &self.filesystem)?;
        }

        // Add dist
        let dist_build = build_dir.join("dist");
        if dist_build.exists() {
            add_directory_to_tar(&mut tar, &dist_build, "dist", &self.filesystem)?;
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
            add_directory_to_tar(&mut tar, &mcp_dir, "mcp", &self.filesystem)?;
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
}

fn add_directory_to_tar<FS: FileSystem>(
    tar: &mut Builder<GzEncoder<fs::File>>,
    dir: &Path,
    prefix: &str,
    _filesystem: &FS,
) -> Result<()> {
    // Recursively collect all files in the directory
    fn collect_all_files(
        dir: &Path,
        files: &mut Vec<std::path::PathBuf>,
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
