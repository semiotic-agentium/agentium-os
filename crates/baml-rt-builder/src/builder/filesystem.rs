//! File system operations implementation

use crate::builder::traits::FileSystem;
use baml_rt_core::{BamlRtError, Result};
use std::fs;
use std::path::Path;

/// Standard file system implementation
#[derive(Clone, Copy)]
pub struct StdFileSystem;

impl FileSystem for StdFileSystem {
    fn copy_dir_all(&self, src: &Path, dst: &Path) -> Result<()> {
        fs::create_dir_all(dst).map_err(BamlRtError::Io)?;

        for entry in fs::read_dir(src).map_err(BamlRtError::Io)? {
            let entry = entry.map_err(BamlRtError::Io)?;
            let path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if path.is_dir() {
                self.copy_dir_all(&path, &dst_path)?;
            } else {
                fs::copy(&path, &dst_path).map_err(BamlRtError::Io)?;
            }
        }

        Ok(())
    }

    fn collect_ts_js_files(&self, dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir).map_err(BamlRtError::Io)? {
            let entry = entry.map_err(BamlRtError::Io)?;
            let path = entry.path();

            if path.is_dir() {
                self.collect_ts_js_files(&path, files)?;
            } else if let Some(ext) = path.extension()
                && (ext == "ts" || ext == "tsx" || ext == "js" || ext == "jsx")
            {
                files.push(path);
            }
        }

        Ok(())
    }

    fn collect_ts_files(&self, dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir).map_err(BamlRtError::Io)? {
            let entry = entry.map_err(BamlRtError::Io)?;
            let path = entry.path();

            if path.is_dir() {
                self.collect_ts_files(&path, files)?;
            } else if let Some(ext) = path.extension()
                && (ext == "ts" || ext == "tsx")
            {
                files.push(path);
            }
        }

        Ok(())
    }

    fn create_dir_all(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir).map_err(BamlRtError::Io)?;
        Ok(())
    }

    fn read_to_string(&self, path: &Path) -> Result<String> {
        fs::read_to_string(path).map_err(BamlRtError::Io)
    }

    fn write_string(&self, path: &Path, contents: &str) -> Result<()> {
        fs::write(path, contents).map_err(BamlRtError::Io)?;
        Ok(())
    }
}
