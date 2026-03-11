//! Agent builder utilities.

pub mod builder;

use std::path::Path;

pub use builder::*;

use crate::builder::error::Result;

/// Build an agent package from an agent directory into a tar.gz file.
/// Uses the same pipeline as the CLI `package` command (type gen, compile, package).
/// Call this from tests instead of spawning `cargo run` to avoid Cargo lock deadlock.
pub async fn build_agent_package(
    agent_dir: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<()> {
    let agent_dir = AgentDir::new(agent_dir.as_ref().to_path_buf())?;
    let build_dir = BuildDir::new()?;

    let filesystem = StdFileSystem;
    filesystem.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;

    let ts_compiler = TscCompiler::new();
    let type_generator = RuntimeTypeGenerator::new();
    let packager = StdPackager::new(filesystem);

    let builder_service = BuilderService::new(ts_compiler, type_generator, packager);
    builder_service
        .build_package(&agent_dir, &build_dir, output.as_ref())
        .await
}
