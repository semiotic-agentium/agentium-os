use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use baml_rt_builder::builder::{BuildDir, RuntimeTypeGenerator, TypeGenerator};
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

fn copy_runtime_d_ts(build_dir: &BuildDir, dest_src: &Path) -> Result<()> {
    let d_ts_src = build_dir.join("dist").join("baml-runtime.d.ts");
    let d_ts_dest = dest_src.join("baml-runtime.d.ts");
    if !d_ts_src.exists() {
        bail!("baml-runtime.d.ts was not generated");
    }
    std::fs::create_dir_all(dest_src)?;
    let data = std::fs::read(&d_ts_src)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dest_src)?;
    tmp.write_all(&data)?;
    tmp.persist(&d_ts_dest).map_err(|e| e.error)?;
    Ok(())
}

async fn regen_fixture(root: &Path) -> Result<()> {
    let baml_src = root.join("baml_src");
    let src_dir = root.join("src");

    let build_dir = BuildDir::new()?;
    let generator = RuntimeTypeGenerator::new();
    generator.generate(&baml_src, &build_dir).await?;
    copy_runtime_d_ts(&build_dir, &src_dir)?;
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
