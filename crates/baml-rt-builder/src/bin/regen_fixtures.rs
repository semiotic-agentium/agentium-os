// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, RuntimeTypeGenerator, TypeGenerator,
    baml_gen::{
        CATALOG_SIDECAR_FILE, GENERATED_BAML_PRELUDE_FILE, is_managed_generated_baml_filename,
    },
    compiler::write_canonical_tsconfig,
};
use baml_rt_tools_claude as _; // Force link so claude tool metadata is in inventory
#[cfg(feature = "security-eval")]
use baml_tools_security_eval as _; // Force link so security-eval tool metadata is in inventory
#[cfg(feature = "slack")]
use baml_tools_slack as _; // Force link so slack tool metadata is in inventory
#[cfg(feature = "slack-notify")]
use baml_tools_slack_notify as _; // Force link so slack-notify tool metadata is in inventory
use baml_tools_system as _; // Force link so system tool metadata is in inventory
use clap::Parser;

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

#[derive(Debug, Parser)]
#[command(
    name = "regen_fixtures",
    about = "Regenerate baml-runtime.d.ts and _baml_runtime.baml for agent directories"
)]
struct Args {
    /// Explicit agent directory path. Repeat to target multiple agents.
    ///
    /// When omitted, scans tests/fixtures/agents and agents.
    #[arg(long = "path", value_name = "AGENT_DIR")]
    paths: Vec<PathBuf>,
}

/// Strip trailing CR/LF and append exactly one `\n` so pre-commit `end-of-file-fixer` agrees with regen.
fn normalize_unix_text_eof(mut bytes: Vec<u8>) -> Vec<u8> {
    while matches!(bytes.last().copied(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    bytes.push(b'\n');
    bytes
}

/// Sync builder output back into the agent's source `baml_src/`.
///
/// Three categories are handled in one pass so the on-disk source tree always matches what
/// the runtime loads at package-time:
///
/// 1. **Generated managed files** (`_baml_runtime.baml`, `_baml_tool_schema_catalog.txt`) —
///    overwritten unconditionally and tracked so any stale managed names left over from prior
///    builds are removed.
/// 2. **Rewritten authored `.baml` prompts** — the prompt-compositor pipeline mutates author
///    prompt bodies in `build_dir/baml_src/` to inject the canonical structured prefix
///    (`tool_schema_prelude` if-block, transcript if-block, narrowed `ctx.output_format`).
///    Syncing those back keeps source-of-truth in lockstep with what the runtime sees so
///    `setup_baml_runtime_from_fixture` callers exercise the canonical prompts. The compositor
///    is **idempotent**, so re-running regen on already-rewritten files preserves author edits
///    inside the task body without duplicating the canonical scaffolding.
/// 3. **Stale managed names** (e.g. legacy `generated_*.baml`) — removed if no longer emitted.
fn sync_generated_baml_files(build_dir: &BuildDir, dest_baml_src: &Path) -> Result<()> {
    let generated_src_dir = build_dir.join("baml_src");
    if !generated_src_dir.is_dir() {
        bail!(
            "Expected generated baml_src directory in build output: {}",
            generated_src_dir.display()
        );
    }

    std::fs::create_dir_all(dest_baml_src)?;

    let mut managed_names: HashSet<String> = HashSet::new();
    for entry in std::fs::read_dir(&generated_src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let is_managed_generated =
            file_name == GENERATED_BAML_PRELUDE_FILE || file_name == CATALOG_SIDECAR_FILE;
        let is_authored_baml = !is_managed_generated && file_name.ends_with(".baml");
        if !is_managed_generated && !is_authored_baml {
            continue;
        }

        if is_managed_generated {
            managed_names.insert(file_name.to_string());
        }
        let data = normalize_unix_text_eof(std::fs::read(&path)?);
        let mut tmp = tempfile::NamedTempFile::new_in(dest_baml_src)?;
        tmp.write_all(&data)?;
        let dest_path = dest_baml_src.join(file_name);
        tmp.persist(&dest_path).map_err(|e| e.error)?;
    }
    for entry in std::fs::read_dir(dest_baml_src)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_managed_generated_baml_filename(file_name) && !managed_names.contains(file_name) {
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
    // Sync single `_baml_runtime.baml` prelude into baml_src; strip legacy split generated files.
    sync_generated_baml_files(&build_dir, &agent_dir.baml_src())?;
    Ok(())
}

fn validate_explicit_agent_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("Agent path does not exist: {}", path.display());
    }
    if !path.is_dir() {
        bail!("Agent path is not a directory: {}", path.display());
    }
    if !path.join("baml_src").is_dir() {
        bail!(
            "Not an agent directory (missing baml_src/): {}",
            path.display()
        );
    }
    Ok(())
}

/// Scan agent roots for directories containing `baml_src/`
/// and regenerate `src/baml-runtime.d.ts` for each.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if !args.paths.is_empty() {
        let mut unique_paths = HashSet::new();
        for raw_path in &args.paths {
            let canonical = raw_path.canonicalize().with_context(|| {
                format!(
                    "Failed to canonicalize agent path '{}'; check that it exists and is readable",
                    raw_path.display()
                )
            })?;
            validate_explicit_agent_dir(&canonical)?;
            unique_paths.insert(canonical);
        }

        let mut paths: Vec<PathBuf> = unique_paths.into_iter().collect();
        paths.sort();

        for path in &paths {
            eprintln!("regen_fixtures: {}", path.display());
            regen_fixture(path)
                .await
                .with_context(|| format!("Failed to regen {}", path.display()))?;
        }
        return Ok(());
    }

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
            if let Err(e) = regen_fixture(&entry.path()).await {
                eprintln!(
                    "{label}: WARN: skipping {} — {:#}",
                    name.to_string_lossy(),
                    e
                );
            }
        }
    }
    Ok(())
}
