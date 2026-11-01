// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `karak tool new` — scaffold a v2 TypeScript external tool.
//!
//! One blessed path (`plans/external-tools-v2.md` §3.1): a typed `defineTool`
//! function authored against `@karakara/tool`. The scaffold is a four-file
//! project; the descriptor and manifest are derived at `karak tool build`, so
//! there is no `tool-manifest.json`, `tool-schema.json`, `tool-server`, or any
//! `runtime`/`access`/`sandbox` knob (all retired in v2).

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use baml_rt_tools::BundleName;
use console::style;

use crate::{
    templates::external_tool::{GeneratedFile, lang, tool_id},
    transaction::TransactionalWriter,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Apply,
    Confirm,
    DryRun,
}

/// Build the v2 TS scaffold file set (exposed for tests).
pub fn build_file_set(name: &str, bundle: &str, description: &str) -> Vec<GeneratedFile> {
    lang::typescript::generate(name, bundle, description)
}

/// Core scaffolder entrypoint.
pub fn run(
    name: &str,
    bundle: &str,
    description: &str,
    output: Option<&str>,
    mode: RunMode,
) -> Result<()> {
    validate_tool_name(name)?;
    BundleName::new(bundle).map_err(|e| {
        anyhow!("Error: invalid bundle '{bundle}': {e}\nHint: use a non-empty name without '/'.")
    })?;

    let output_dir = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(name));
    validate_output_dir(&output_dir)?;

    let default_description = format!("{} external tool", capitalize_first(name));
    let description = if description.trim().is_empty() {
        default_description.as_str()
    } else {
        description.trim()
    };

    let files = build_file_set(name, bundle, description);

    if matches!(mode, RunMode::Confirm | RunMode::DryRun) {
        println!();
        println!("{}", style("Summary:").bold());
        println!("  Name:     {}", style(name).cyan());
        println!("  Tool ID:  {}", style(tool_id(bundle, name)).cyan());
        println!("  Language: {}", style("typescript").cyan());
        println!("  Output:   {}", style(output_dir.display()).cyan());
        println!("  Desc:     {}", style(description).cyan());
    }

    let mut writer = TransactionalWriter::new();
    writer.stage_mkdir(&output_dir);
    for file in &files {
        writer.stage_create(
            output_dir.join(&file.relative_path),
            file.content.as_bytes(),
        )?;
    }

    println!();
    println!("{}", style("Operations to perform:").bold());
    for line in writer.summary() {
        println!("  {line}");
    }

    match mode {
        RunMode::DryRun => {
            println!();
            println!(
                "{}",
                style("Dry run successful - validation passed, no changes made.").yellow()
            );
            writer.discard();
            return Ok(());
        }
        RunMode::Confirm => {
            println!();
            if !crate::interactive::confirm_proceed()? {
                println!("{}", style("Aborted - no changes made.").yellow());
                writer.discard();
                return Ok(());
            }
        }
        RunMode::Apply => {}
    }

    println!();
    println!("{}", style("Applying changes...").cyan());
    writer.commit().map_err(|e| {
        anyhow!(
            "Error: failed to apply scaffold files.\nCause: {e}\nHint: verify write permissions."
        )
    })?;

    println!();
    println!(
        "{} Created external tool scaffold: {}",
        style("✓").green(),
        style(name).bold()
    );
    println!("  Path: {}", style(output_dir.display()).cyan());
    println!();
    println!("{}", style("Next steps:").bold());
    println!("  1. Edit src/tool.ts (one typed defineTool function).");
    println!("  2. {}", style("npm install && karak tool build").cyan());
    println!("  3. {}", style("karak tool test").cyan());

    Ok(())
}

/// CLI-facing entry: resolve interactive prompts then scaffold.
pub struct ToolNewCli {
    pub name: Option<String>,
    pub bundle: Option<String>,
    pub description: Option<String>,
    pub output: Option<String>,
    pub dry_run: bool,
}

pub fn run_cli(cmd: ToolNewCli) -> Result<()> {
    use crate::{interactive, templates::external_tool::DEFAULT_BUNDLE};

    let interactive_mode = cmd.name.is_none();
    let name = match cmd.name {
        Some(n) => n,
        None => interactive::prompt_tool_name()?,
    };
    let bundle = match cmd.bundle {
        Some(b) => b,
        None if interactive_mode => interactive::prompt_external_tool_bundle()?,
        None => DEFAULT_BUNDLE.to_string(),
    };
    let description = match cmd.description {
        Some(d) => d,
        None if interactive_mode => interactive::prompt_tool_description()?,
        None => String::new(),
    };
    let output = match cmd.output {
        Some(o) => Some(o),
        None if interactive_mode => Some(interactive::prompt_external_tool_output(&name)?),
        None => None,
    };
    let mode = if interactive_mode {
        RunMode::Confirm
    } else if cmd.dry_run {
        RunMode::DryRun
    } else {
        RunMode::Apply
    };
    run(&name, &bundle, &description, output.as_deref(), mode)
}

fn validate_output_dir(output_dir: &Path) -> Result<()> {
    if output_dir.exists() {
        if !output_dir.is_dir() {
            bail!(
                "Error: output path exists and is not a directory: {}",
                output_dir.display()
            );
        }
        if fs::read_dir(output_dir)?.count() > 0 {
            bail!(
                "Error: output directory already exists and is non-empty: {}\nHint: pass --output <new-dir> or clean the directory.",
                output_dir.display()
            );
        }
    }
    Ok(())
}

fn validate_tool_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!(
            "Error: tool name cannot be empty.\nHint: use lowercase like `weather` or `clickup-sync`."
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        bail!(
            "Error: invalid tool name '{name}'.\nHint: lowercase letters, numbers, underscores, or hyphens only."
        );
    }
    if name.starts_with('_')
        || name.ends_with('_')
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("__")
        || name.contains("--")
    {
        bail!(
            "Error: invalid tool name '{name}'.\nHint: avoid leading/trailing separators and consecutive '_' or '-'."
        );
    }
    Ok(())
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(files: &[GeneratedFile], path: &str) -> bool {
        files.iter().any(|f| f.relative_path == path)
    }

    #[test]
    fn scaffold_emits_v2_typescript_layout() {
        let files = build_file_set("weather", "support", "Weather tool");
        assert!(has(&files, "package.json"));
        assert!(has(&files, "tsconfig.json"));
        assert!(has(&files, "src/tool.ts"));
        assert!(has(&files, "README.md"));
        // No v1 artifacts.
        assert!(!has(&files, "tool-server"));
        assert!(!has(&files, "tool-manifest.json"));
        assert!(!has(&files, "src/main.ts"));
    }

    #[test]
    fn scaffold_references_the_sdk_and_tool_id() {
        let files = build_file_set("weather", "support", "Weather tool");
        let pkg = files
            .iter()
            .find(|f| f.relative_path == "package.json")
            .unwrap();
        assert!(pkg.content.contains("@karakara/tool"));
        let tool = files
            .iter()
            .find(|f| f.relative_path == "src/tool.ts")
            .unwrap();
        assert!(tool.content.contains("defineTool"));
        assert!(tool.content.contains("support/weather"));
    }

    #[test]
    fn rejects_bad_names() {
        assert!(validate_tool_name("").is_err());
        assert!(validate_tool_name("Weather").is_err());
        assert!(validate_tool_name("-x").is_err());
        assert!(validate_tool_name("a--b").is_err());
        assert!(validate_tool_name("clickup-sync").is_ok());
    }
}
