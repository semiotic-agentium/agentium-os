use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, RuntimeTypeGenerator, TypeGenerator, compiler::write_canonical_tsconfig,
};
use baml_rt_tools_claude as _; // Force link so claude tool metadata is in inventory
#[cfg(feature = "security-eval")]
use baml_tools_security_eval as _; // Force link so security-eval tool metadata is in inventory
#[cfg(feature = "slack")]
use baml_tools_slack as _; // Force link so slack tool metadata is in inventory
use baml_tools_system as _; // Force link so system tool metadata is in inventory

fn fixture_agents_dir() -> Result<PathBuf> {
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

fn production_agents_dir() -> Result<PathBuf> {
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

    Ok(workspace_root.join("agents"))
}

fn sync_generated_baml_files(build_dir: &BuildDir, dest_baml_src: &Path) -> Result<()> {
    let generated_src_dir = build_dir.join("baml_src");
    if !generated_src_dir.is_dir() {
        bail!(
            "Expected generated baml_src directory in build output: {}",
            generated_src_dir.display()
        );
    }

    std::fs::create_dir_all(dest_baml_src)?;

    let mut generated_names: HashSet<String> = HashSet::new();
    for entry in std::fs::read_dir(&generated_src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !file_name.starts_with("generated_") || !file_name.ends_with(".baml") {
            continue;
        }

        generated_names.insert(file_name.to_string());
        let data = std::fs::read(&path)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dest_baml_src)?;
        tmp.write_all(&data)?;
        let dest_path = dest_baml_src.join(file_name);
        tmp.persist(&dest_path).map_err(|e| e.error)?;
    }
    // Remove stale generated_*.baml files that are no longer emitted by the builder.
    for entry in std::fs::read_dir(dest_baml_src)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.starts_with("generated_")
            && file_name.ends_with(".baml")
            && !generated_names.contains(file_name)
        {
            std::fs::remove_file(path)?;
        }
    }

    Ok(())
}

async fn regen_fixture(root: &Path) -> Result<()> {
    // Ensure canonical tsconfig.json
    write_canonical_tsconfig(root)?;

    let agent_dir = AgentDir::new(root.to_path_buf())?;
    let build_dir = BuildDir::new()?;
    let generator = RuntimeTypeGenerator::new();
    // generate() writes src/baml-runtime.d.ts directly into the agent's source tree.
    generator.generate(&agent_dir, &build_dir).await?;
    // Sync generated_*.baml tool interfaces back into the agent's baml_src.
    sync_generated_baml_files(&build_dir, &agent_dir.baml_src())?;
    Ok(())
}

/// Scan agent roots for directories containing `baml_src/`
/// and regenerate `src/baml-runtime.d.ts` for each.
#[tokio::main]
async fn main() -> Result<()> {
    let roots = vec![
        ("regen_fixtures", fixture_agents_dir()?),
        ("regen_agents", production_agents_dir()?),
    ];

    for (label, dir) in roots {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("baml_src").is_dir())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in &entries {
            let name = entry.file_name();
            eprintln!("{label}: {}", name.to_string_lossy());
            regen_fixture(&entry.path()).await?;
        }
    }
    Ok(())
}
