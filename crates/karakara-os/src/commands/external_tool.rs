// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! External-tool snapshot inspection. Tools are built and published with
//! `karak tool build` / `karak tool publish`.

use std::path::PathBuf;

use anyhow::{Result, bail};
use baml_rt_tools::external_tool_cache;
use console::style;
use serde_json::json;

pub fn inspect(name: &str, cache_dir: Option<&str>, json_output: bool) -> Result<()> {
    let cache_root = cache_root(cache_dir)?;
    let mut snapshots = Vec::new();

    if let Ok(path) = external_tool_cache::approved_snapshot_path(&cache_root, name)
        && path.exists()
    {
        snapshots.push(external_tool_cache::read_snapshot(&path)?);
    }
    for snap in external_tool_cache::read_pending_snapshots(&cache_root)? {
        if snap.tool.name == name {
            snapshots.push(snap);
        }
    }

    if snapshots.is_empty() {
        bail!(
            "no external tool snapshots found for '{name}' in {}",
            cache_root.display()
        );
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "snapshots": snapshots }))?
        );
        return Ok(());
    }

    println!("{} {}", style("External tool:").bold(), style(name).cyan());
    for snap in snapshots {
        println!(
            "  state={:?} snapshot={} schema={} runtime={} created_at={}",
            snap.approval.state,
            snap.snapshot_digest,
            snap.digests.schema_digest,
            snap.digests.runtime_digest,
            snap.created_at,
        );
        println!("    manifest={}", snap.digests.manifest_digest);
    }
    Ok(())
}

fn cache_root(cache_dir: Option<&str>) -> Result<PathBuf> {
    if let Some(dir) = cache_dir {
        return Ok(PathBuf::from(dir));
    }
    Ok(std::env::current_dir()?.join(".baml-external-tool-cache"))
}
