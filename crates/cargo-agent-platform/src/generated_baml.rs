use std::{collections::HashSet, path::Path};

use anyhow::Result;
use baml_rt_core::atomic_io::atomic_write;

/// Sync generated runtime BAML files from build_dir to agent's baml_src.
pub fn sync_generated_baml_files(
    build_dir: &baml_rt_builder::builder::BuildDir,
    dest_baml_src: &Path,
) -> Result<()> {
    let generated_src_dir = build_dir.join("baml_src");
    if !generated_src_dir.is_dir() {
        // No generated files to sync - this is not an error
        return Ok(());
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
        let should_sync = (file_name.starts_with("generated_") && file_name.ends_with(".baml"))
            || file_name == "_baml_runtime.baml";
        if !should_sync {
            continue;
        }

        if file_name.starts_with("generated_") && file_name.ends_with(".baml") {
            generated_names.insert(file_name.to_string());
        }
        let data = std::fs::read(&path)?;
        let dest_path = dest_baml_src.join(file_name);
        atomic_write(&dest_path, &data)?;
    }

    // Remove stale generated_*.baml files that are no longer emitted by the builder
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
