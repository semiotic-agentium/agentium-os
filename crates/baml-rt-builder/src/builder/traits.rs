// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Traits for builder operations
//!
//! These traits provide a clean abstraction for different operations
//! in the agent building pipeline, enabling testability and modularity.

use std::path::Path;

use crate::builder::{
    error::Result,
    types::{AgentDir, BuildDir},
};

/// Trait for compiling TypeScript to JavaScript.
///
/// Implementations receive the full `AgentDir` (so they can locate the agent's
/// `tsconfig.json` and `src/`) and a `dist_dir` for compiled output.
#[async_trait::async_trait]
pub trait TypeScriptCompiler: Send + Sync {
    /// Compile TypeScript files from the agent's `src/` to `dist_dir`.
    async fn compile(&self, agent_dir: &AgentDir, dist_dir: &Path) -> Result<()>;
}

/// Trait for generating runtime type declarations.
///
/// Implementations receive the full `AgentDir` so they can write generated
/// files (e.g. `src/baml-runtime.d.ts`) directly into the agent tree.
#[async_trait::async_trait]
pub trait TypeGenerator: Send + Sync {
    /// Generate TypeScript type declarations and BAML tool interfaces.
    async fn generate(&self, agent_dir: &AgentDir, build_dir: &BuildDir) -> Result<()>;
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
