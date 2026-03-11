use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, RuntimeTypeGenerator, TypeGenerator, compiler::TSCONFIG_JSON,
};
use baml_rt_tools_claude as _; // Force link so claude tool metadata is in inventory
#[cfg(feature = "slack")]
use baml_tools_slack as _; // Force link so slack tool metadata is in inventory
use baml_tools_system as _; // Force link so system tool metadata is in inventory

fn agents_dir() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .with_context(|| {
            format!(
                "Could not determine workspace root from manifest dir: {}",
                manifest_dir.display()
            )
        })?;

    Ok(workspace_root.join("tests").join("fixtures").join("agents"))
}

async fn regen_fixture(root: &Path) -> Result<()> {
    // Ensure canonical tsconfig.json
    std::fs::write(root.join("tsconfig.json"), TSCONFIG_JSON)?;

    let agent_dir = AgentDir::new(root.to_path_buf())?;
    let build_dir = BuildDir::new()?;
    let generator = RuntimeTypeGenerator::new();
    // This writes src/baml-runtime.d.ts directly into the agent's source tree.
    generator.generate(&agent_dir, &build_dir).await?;
    Ok(())
}

/// Scan `tests/fixtures/agents/` for directories containing `baml_src/`
/// and regenerate `src/baml-runtime.d.ts` for each.
#[tokio::main]
async fn main() -> Result<()> {
    let dir = agents_dir()?;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("baml_src").is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let name = entry.file_name();
        eprintln!("regen_fixtures: {}", name.to_string_lossy());
        regen_fixture(&entry.path()).await?;
    }
    Ok(())
}
