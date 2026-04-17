//! `new-tool` subcommand implementation (default, external-tool path).
//!
//! Creates a standalone external tool scaffold in its own directory —
//! not compiled into the platform workspace. The runner picks the tool up via
//! `BAML_EXTERNAL_TOOLS_DIR` at deploy time.
//!
//! For scaffolding a tool *inside* the platform workspace (system bundles,
//! compiled-in integrations), use `new-static-tool` instead.
//!
//! Most stringly-typed knobs were lifted into typed enums
//! ([`Access`], [`Language`]) so clap validates user input and the template
//! dispatcher gets an exhaustive `match`. Bundle stays free-form by design.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use baml_rt_tools::BundleName;
use console::style;

use crate::{
    templates::external_tool::{
        Access, GeneratedFile, Language, ScaffoldContext, metadata_json, readme_md,
    },
    transaction::TransactionalWriter,
};

/// Execution mode for the scaffolder. Collapses the two `bool` flags that used
/// to be carried around separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Write files after printing the summary. Default CLI behaviour.
    Apply,
    /// Print the plan, prompt for confirmation before writing.
    Confirm,
    /// Print the plan and stop; never touch the filesystem.
    DryRun,
}

impl RunMode {
    fn shows_summary(self) -> bool {
        matches!(self, Self::Confirm | Self::DryRun)
    }
}

pub fn run(
    name: &str,
    bundle: &str,
    lang: Language,
    access: Access,
    description: &str,
    output: Option<&str>,
    mode: RunMode,
) -> Result<()> {
    validate_tool_name(name)?;
    // Reuse the runtime's BundleName rule (non-empty, no '/') so the scaffolder
    // can't produce a tool-metadata.json the runner would later reject.
    BundleName::new(bundle).map_err(|e| {
        anyhow!("Error: invalid bundle '{bundle}': {e}\nHint: use a non-empty name without '/'.")
    })?;

    let output_dir = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(name));
    validate_output_dir(&output_dir)?;

    let default_description = format!("{} external tool for BAML runtime", capitalize_first(name));
    let description = if description.trim().is_empty() {
        default_description.as_str()
    } else {
        description.trim()
    };

    let ctx = ScaffoldContext {
        name,
        bundle,
        access,
        language: lang,
        description,
    };

    let files = build_file_set(&ctx);

    if mode.shows_summary() {
        println!();
        println!("{}", style("Summary:").bold());
        println!("  Name:      {}", style(name).cyan());
        println!("  Tool ID:   {}", style(ctx.tool_id()).cyan());
        println!("  Language:  {}", style(lang.as_str()).cyan());
        println!("  Access:    {}", style(access.as_str()).cyan());
        println!("  Output:    {}", style(output_dir.display()).cyan());
        println!("  Desc:      {}", style(description).cyan());
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
            "Error: failed to apply scaffold files.\nCause: {e}\nHint: verify write permissions and output path."
        )
    })?;

    set_executable_bits(&output_dir, &files)?;

    println!();
    println!(
        "{} Created external tool scaffold: {}",
        style("✓").green(),
        style(name).bold()
    );
    println!("  Path: {}", style(output_dir.display()).cyan());
    println!();
    println!("{}", style("Next steps:").bold());
    println!("  1. Implement logic in scaffolded language files");
    println!(
        "  2. Run setup/build instructions in {}",
        style(output_dir.join("README.md").display()).cyan()
    );
    println!(
        "  3. Set {} and run an agent that references {}",
        style("BAML_EXTERNAL_TOOLS_DIR").cyan(),
        style(ctx.tool_id()).cyan()
    );

    Ok(())
}

/// Compose metadata + README + language-specific files into the final set.
///
/// Exposed so tests can snapshot-check the scaffold without touching the CLI.
pub fn build_file_set(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    let mut files = vec![
        GeneratedFile::new("tool-metadata.json", metadata_json::generate(ctx)),
        GeneratedFile::new("README.md", readme_md::generate(ctx)),
    ];
    files.extend(ctx.language.files(ctx));
    files
}

fn validate_output_dir(output_dir: &Path) -> Result<()> {
    if output_dir.exists() {
        if !output_dir.is_dir() {
            bail!(
                "Error: output path exists and is not a directory: {}",
                output_dir.display()
            );
        }
        let entries = fs::read_dir(output_dir)?;
        if entries.count() > 0 {
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
            "Error: tool name cannot be empty.\nHint: use kebab-case like `echo` or `clickup-sync`."
        );
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!(
            "Error: invalid tool name '{}'.\nHint: use kebab-case with lowercase letters, numbers, and hyphens only.",
            name
        );
    }

    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        bail!(
            "Error: invalid tool name '{}'.\nHint: avoid leading/trailing/consecutive hyphens.",
            name
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

fn set_executable_bits(output_dir: &Path, files: &[GeneratedFile]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for file in files.iter().filter(|f| f.executable) {
            let path = output_dir.join(&file.relative_path);
            let metadata = fs::metadata(&path)?;
            let mut perms = metadata.permissions();
            let mode = perms.mode();
            perms.set_mode(mode | 0o111);
            fs::set_permissions(path, perms)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use baml_rt_tools::external_tools::metadata_catalog::ExternalMetadataCatalog;

    use super::*;

    fn ctx<'a>(lang: Language) -> ScaffoldContext<'a> {
        ScaffoldContext {
            name: "echo",
            bundle: "dev",
            access: Access::Read,
            language: lang,
            description: "Echo external tool",
        }
    }

    fn assert_scaffold_has(files: &[GeneratedFile], path: &str) {
        assert!(
            files.iter().any(|f| f.relative_path == path),
            "scaffold missing expected file `{path}`; got {:?}",
            files.iter().map(|f| &f.relative_path).collect::<Vec<_>>()
        );
    }

    /// Golden fixtures per language — catches silent drift when adding a
    /// language or renaming a scaffolded file.
    #[test]
    fn scaffold_contains_expected_files() {
        let rust_files = build_file_set(&ctx(Language::Rust));
        assert_scaffold_has(&rust_files, "tool-metadata.json");
        assert_scaffold_has(&rust_files, "README.md");
        assert_scaffold_has(&rust_files, "Cargo.toml");
        assert_scaffold_has(&rust_files, "src/main.rs");
        assert_scaffold_has(&rust_files, "tool-server");

        let bash_files = build_file_set(&ctx(Language::Bash));
        assert_scaffold_has(&bash_files, "tool-metadata.json");
        assert_scaffold_has(&bash_files, "tool-server");

        let py_files = build_file_set(&ctx(Language::Python));
        assert_scaffold_has(&py_files, "main.py");
        assert_scaffold_has(&py_files, "tool-server");

        let ts_files = build_file_set(&ctx(Language::Typescript));
        assert_scaffold_has(&ts_files, "package.json");
        assert_scaffold_has(&ts_files, "tsconfig.json");
        assert_scaffold_has(&ts_files, "src/main.ts");
        assert_scaffold_has(&ts_files, "tool-server");
    }

    /// `tool-metadata.json` must round-trip through the runtime's catalog
    /// loader — this is the hard invariant that prevents silent drift between
    /// scaffold output and the runtime's `RawToolMetadata` parser.
    #[test]
    fn scaffolded_metadata_is_loadable_by_runtime_catalog() {
        for lang in [
            Language::Rust,
            Language::Bash,
            Language::Python,
            Language::Typescript,
        ] {
            let ctx = ctx(lang);
            let files = build_file_set(&ctx);

            let tmp = tempfile::tempdir().expect("tmp dir");
            for file in &files {
                // Only metadata + server matter for this check; skip README.
                if file.relative_path == "tool-metadata.json" {
                    std::fs::write(
                        tmp.path().join(&file.relative_path),
                        file.content.as_bytes(),
                    )
                    .unwrap();
                }
            }

            let catalog = ExternalMetadataCatalog::from_dirs(&[tmp.path().to_path_buf()])
                .unwrap_or_else(|e| {
                    panic!("{lang:?} metadata should load via catalog: {e}");
                });
            assert_eq!(catalog.len(), 1, "{lang:?} catalog should have one tool");
        }
    }

    /// Generated metadata JSON parses as valid JSON (no bad escapes from
    /// control chars, quotes, etc.).
    #[test]
    fn scaffolded_metadata_is_valid_json() {
        let mut desc_with_specials = String::from("weird ");
        desc_with_specials.push('\u{0007}'); // control char
        desc_with_specials.push('"');
        desc_with_specials.push('\\');
        desc_with_specials.push_str(" text");

        let ctx = ScaffoldContext {
            name: "echo",
            bundle: "dev",
            access: Access::Read,
            language: Language::Bash,
            description: &desc_with_specials,
        };

        let raw = metadata_json::generate(&ctx);
        serde_json::from_str::<serde_json::Value>(&raw)
            .expect("metadata must round-trip through serde_json regardless of description chars");
    }
}
