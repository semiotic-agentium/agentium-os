//! Newtype wrappers for type safety at API boundaries
//!
//! These newtypes provide compile-time guarantees about the validity and meaning
//! of values passed between different parts of the system.

use std::fmt;
use std::path::{Path, PathBuf};

/// Agent directory path - validated to exist and contain required structure
#[derive(Debug, Clone)]
pub struct AgentDir(PathBuf);

impl AgentDir {
    /// Create a new AgentDir, validating that it exists and contains baml_src
    pub fn new(path: PathBuf) -> baml_rt_core::Result<Self> {
        if !path.exists() {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent directory does not exist: {}",
                path.display()
            )));
        }

        let baml_src = path.join("baml_src");
        if !baml_src.exists() {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "baml_src directory not found in {}",
                path.display()
            )));
        }

        Ok(Self(path))
    }

    /// Get the inner path
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Get the baml_src subdirectory
    pub fn baml_src(&self) -> PathBuf {
        self.0.join("baml_src")
    }

    /// Get the src subdirectory
    pub fn src(&self) -> PathBuf {
        self.0.join("src")
    }
}

impl fmt::Display for AgentDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// Package file path - validated to exist and be a tar.gz file
#[derive(Debug, Clone)]
pub struct PackagePath(PathBuf);

impl PackagePath {
    /// Create a new PackagePath, validating it exists and has .tar.gz extension
    pub fn new(path: PathBuf) -> baml_rt_core::Result<Self> {
        if !path.exists() {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Package file does not exist: {}",
                path.display()
            )));
        }

        if path.extension().and_then(|s| s.to_str()) != Some("gz") {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Package file must have .tar.gz extension: {}",
                path.display()
            )));
        }

        Ok(Self(path))
    }

    /// Get the inner path
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for PackagePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// Function name - validated to be non-empty
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionName(String);

impl FunctionName {
    /// Create a new FunctionName, validating it's non-empty
    pub fn new(name: String) -> baml_rt_core::Result<Self> {
        if name.is_empty() {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(
                "Function name cannot be empty".to_string(),
            ));
        }

        Ok(Self(name))
    }

    /// Get the inner string
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FunctionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for FunctionName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Build directory - temporary directory for build artifacts
#[derive(Debug, Clone)]
pub struct BuildDir(PathBuf);

impl BuildDir {
    /// Create a new temporary build directory
    pub fn new() -> baml_rt_core::Result<Self> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(baml_rt_core::BamlRtError::SystemTime)?;

        let build_dir = std::env::temp_dir().join(format!("baml-build-{}", timestamp.as_secs()));
        std::fs::create_dir_all(&build_dir).map_err(baml_rt_core::BamlRtError::Io)?;

        Ok(Self(build_dir))
    }

    /// Get the inner path
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Join a path to this build directory
    pub fn join<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        self.0.join(path)
    }
}

impl fmt::Display for BuildDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}
