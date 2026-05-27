//! Agent builder utilities.

pub mod builder;
pub mod mcp_registry;

use std::path::Path;

use baml_rt_tools_claude as _;
#[cfg(feature = "clickup")]
use baml_tools_clickup as _;
#[cfg(feature = "memory")]
use baml_tools_memory as _;
#[cfg(feature = "notion")]
use baml_tools_notion as _;
#[cfg(feature = "security-eval")]
use baml_tools_security_eval as _;
#[cfg(feature = "slack")]
use baml_tools_slack as _;
use baml_tools_system as _;
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
    // TS 5.6+ / TS 6: explicit rootDir in tsconfig (TS5011). Write before typegen/tsc so every
    // package build is self-contained even if the repo copy of tsconfig.json is stale.
    write_canonical_tsconfig(agent_dir.as_path())?;
    let build_dir = BuildDir::new()?;

    let filesystem = StdFileSystem;
    filesystem.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;

    let ts_compiler = TscCompiler::new();
    let type_generator = RuntimeTypeGenerator::new();
    let packager = StdPackager::new();

    let builder_service = BuilderService::new(ts_compiler, type_generator, packager);
    builder_service
        .build_package(&agent_dir, &build_dir, output.as_ref())
        .await
}
