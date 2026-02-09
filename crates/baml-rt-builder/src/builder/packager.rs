//! Packager implementation for creating tar.gz agent packages

use crate::builder::traits::{FileSystem, Packager};
use crate::builder::types::{AgentDir, BuildDir};
use baml_rt_core::{AgentManifest, BamlRtError, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::fs;
use std::path::Path;
use tar::{Builder, Header};
use uuid::Uuid;

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
            fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }

        let tar_gz = fs::File::create(output).map_err(BamlRtError::Io)?;
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = Builder::new(enc);

        // Add manifest.json (ensure signature exists; use dist/index.js as entry_point when built)
        let manifest_path = agent_dir.as_path().join("manifest.json");
        if manifest_path.exists() {
            let content = fs::read_to_string(&manifest_path).map_err(BamlRtError::Io)?;
            let mut manifest: AgentManifest =
                serde_json::from_str(&content).map_err(BamlRtError::Json)?;
            if manifest.signature.is_empty() {
                manifest.signature = Uuid::new_v4().to_string();
            }
            if build_dir.join("dist").join("index.js").exists() {
                manifest.entry_point = "dist/index.js".to_string();
            }
            let content = serde_json::to_string_pretty(&manifest).map_err(BamlRtError::Json)?;
            let mut header = Header::new_gnu();
            header
                .set_path("manifest.json")
                .map_err(BamlRtError::TarHeaderPath)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, content.as_bytes())
                .map_err(BamlRtError::Io)?;
        }

        // Add package.json if it exists
        let package_json_path = agent_dir.as_path().join("package.json");
        if package_json_path.exists() {
            let content = fs::read_to_string(&package_json_path).map_err(BamlRtError::Io)?;
            let mut header = Header::new_gnu();
            header
                .set_path("package.json")
                .map_err(BamlRtError::TarHeaderPath)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, content.as_bytes())
                .map_err(BamlRtError::Io)?;
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

        tar.finish().map_err(BamlRtError::Io)?;
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
    collect_all_files(dir, &mut files).map_err(BamlRtError::Io)?;

    for file_path in files {
        let content = fs::read_to_string(&file_path).map_err(BamlRtError::Io)?;
        let relative_path = file_path.strip_prefix(dir).map_err(|_| {
            BamlRtError::InvalidArgument(format!(
                "File {} is not under directory {}",
                file_path.display(),
                dir.display()
            ))
        })?;

        let tar_path = format!("{}/{}", prefix, relative_path.display());
        let mut header = Header::new_gnu();
        header
            .set_path(&tar_path)
            .map_err(BamlRtError::TarHeaderPath)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_bytes())
            .map_err(BamlRtError::Io)?;
    }

    Ok(())
}
