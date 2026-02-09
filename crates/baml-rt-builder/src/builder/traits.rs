//! Traits for builder operations
//!
//! These traits provide a clean abstraction for different operations
//! in the agent building pipeline, enabling testability and modularity.

use crate::builder::types::{AgentDir, BuildDir};
use baml_rt_core::Result;
use std::path::Path;

/// Trait for linting source code
#[async_trait::async_trait]
pub trait Linter: Send + Sync {
    /// Lint the source code in the given agent directory
    async fn lint(&self, agent_dir: &AgentDir) -> Result<()>;
}

/// Trait for compiling TypeScript to JavaScript
#[async_trait::async_trait]
pub trait TypeScriptCompiler: Send + Sync {
    /// Compile TypeScript files from source directory to dist directory
    async fn compile(&self, src_dir: &Path, dist_dir: &Path) -> Result<()>;
}

/// Trait for generating runtime type declarations
#[async_trait::async_trait]
pub trait TypeGenerator: Send + Sync {
    /// Generate TypeScript type declarations for runtime host functions
    /// Takes the baml_src directory to load the BAML runtime and discover functions
    async fn generate(&self, baml_src: &Path, build_dir: &BuildDir) -> Result<()>;
}

/// Trait for file system operations
pub trait FileSystem: Send + Sync {
    /// Copy a directory recursively
    fn copy_dir_all(&self, src: &Path, dst: &Path) -> Result<()>;

    /// Collect TypeScript/JavaScript files from a directory
    fn collect_ts_js_files(&self, dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()>;

    /// Collect TypeScript files from a directory
    fn collect_ts_files(&self, dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()>;

    /// Create directories recursively
    fn create_dir_all(&self, dir: &Path) -> Result<()>;

    /// Read a file into a string
    fn read_to_string(&self, path: &Path) -> Result<String>;

    /// Write a string to a file
    fn write_string(&self, path: &Path, contents: &str) -> Result<()>;
}

/// Trait for packaging agents into tar.gz archives
#[async_trait::async_trait]
pub trait Packager: Send + Sync {
    /// Package an agent from build directory to output path
    async fn package(
        &self,
        agent_dir: &AgentDir,
        build_dir: &BuildDir,
        output: &Path,
    ) -> Result<()>;
}
