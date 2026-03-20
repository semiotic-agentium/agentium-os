//! `new-tool` subcommand implementation.
//!
//! Creates a new tool crate and patches all necessary files.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use console::style;

use crate::{
    patchers::{
        Patcher, builder_toml::BuilderTomlPatcher, runner_toml::RunnerTomlPatcher,
        tool_links_lib::ToolLinksLibPatcher, tool_links_toml::ToolLinksTomlPatcher,
        workspace_toml::WorkspaceTomlPatcher,
    },
    templates::{tool_cargo_toml, tool_lib_rs},
    transaction::TransactionalWriter,
};

/// Run the new-tool command.
pub fn run(name: &str, bundle: &str, access: &str, dry_run: bool, interactive: bool) -> Result<()> {
    // Validate inputs
    validate_tool_name(name)?;
    validate_bundle(bundle)?;
    validate_access(access)?;

    // Find workspace root
    let workspace_root = find_workspace_root()?;
    println!(
        "{} workspace root: {}",
        style("Found").green(),
        workspace_root.display()
    );

    // Check if tool already exists
    let tool_dir = workspace_root.join(format!("crates/tools/{name}"));
    if tool_dir.exists() {
        bail!(
            "Error: tool crate already exists at {}\nHint: choose a different tool name or remove the existing directory.\nNext step: run `cargo agent-platform list-tools` to inspect existing tool IDs.",
            tool_dir.display()
        );
    }

    // Prepare the transaction
    let mut writer = TransactionalWriter::new();

    // 1. Create tool crate directory and files
    let tool_src_dir = tool_dir.join("src");
    writer.stage_mkdir(&tool_dir);
    writer.stage_mkdir(&tool_src_dir);

    let description = format!("{} tool for BAML runtime", capitalize_first(name));
    let cargo_toml_content = tool_cargo_toml::generate(name, &description);
    let lib_rs_content = tool_lib_rs::generate(name, bundle, access);

    writer
        .stage_create(tool_dir.join("Cargo.toml"), cargo_toml_content)
        .context("Failed to stage crates/tools/<name>/Cargo.toml for creation")?;
    writer
        .stage_create(tool_src_dir.join("lib.rs"), lib_rs_content)
        .context("Failed to stage crates/tools/<name>/src/lib.rs for creation")?;

    // 2. Patch workspace Cargo.toml
    patch_file(&mut writer, &workspace_root, &WorkspaceTomlPatcher, name)?;

    // 3. Patch baml-tool-links/Cargo.toml
    patch_file(&mut writer, &workspace_root, &ToolLinksTomlPatcher, name)?;

    // 4. Patch baml-tool-links/src/lib.rs
    patch_file(&mut writer, &workspace_root, &ToolLinksLibPatcher, name)?;

    // 5. Patch baml-agent-runner/Cargo.toml
    patch_file(&mut writer, &workspace_root, &RunnerTomlPatcher, name)?;

    // 6. Patch baml-rt-builder/Cargo.toml
    patch_file(&mut writer, &workspace_root, &BuilderTomlPatcher, name)?;

    // Show summary (for interactive or dry-run mode)
    if interactive || dry_run {
        println!();
        println!("{}", style("Summary:").bold());
        println!("  Name:   {}", style(name).cyan());
        println!("  Bundle: {}", style(bundle).cyan());
        println!("  Access: {}", style(access).cyan());
    }

    println!();
    println!("{}", style("Operations to perform:").bold());
    for line in writer.summary() {
        println!("  {line}");
    }

    // Show note about complex tools
    println!();
    println!("{}", style("Note:").yellow().bold());
    println!("  Most tools are auto-registered via inventory and need no extra setup.");
    println!("  If your tool requires runtime context (e.g., agent name, manifest data),");
    println!("  you'll also need to manually edit:");
    println!(
        "    - {}",
        style("crates/baml-agent-runner/src/optional_tool_bundles.rs").cyan()
    );
    println!(
        "    - {}",
        style("crates/baml-rt-builder/src/optional_tool_bundles.rs").cyan()
    );
    println!("  See the {} tool for an example.", style("memory").cyan());

    if dry_run {
        println!();
        println!(
            "{}",
            style("Dry run successful - validation passed, no changes made.").yellow()
        );
        writer.discard();
        return Ok(());
    }

    // In interactive mode, ask for confirmation
    if interactive {
        println!();
        if !crate::interactive::confirm_proceed()? {
            println!("{}", style("Aborted - no changes made.").yellow());
            writer.discard();
            return Ok(());
        }
    }

    // Commit the transaction
    println!();
    println!("{}", style("Applying changes...").cyan());
    writer.commit().map_err(|e| {
        anyhow!(
            "Error: failed to apply staged changes.\nCause: {e}\nHint: this can happen when workspace files drift from expected structure.\nNext step: run `cargo agent-platform doctor --ci` and retry."
        )
    })?;

    println!();
    println!(
        "{} Created tool crate: {}",
        style("✓").green(),
        style(name).bold()
    );
    println!();
    println!("{}", style("Next steps:").bold());
    println!(
        "  1. Edit {} to implement your tool logic",
        style(format!("crates/tools/{name}/src/lib.rs")).cyan()
    );
    println!(
        "  2. Run {} to verify compilation",
        style(format!("cargo check -p baml-tools-{name}")).cyan()
    );
    println!(
        "  3. Run {} to update generated files",
        style("cargo run -p baml-rt-builder --bin regen_fixtures").cyan()
    );

    println!();
    println!("{}", style("Note:").yellow().bold());
    println!("  If your tool requires runtime context (e.g., agent name, manifest data),");
    println!("  you'll need to manually add initialization code to:");
    println!(
        "    - {}",
        style("crates/baml-agent-runner/src/optional_tool_bundles.rs").cyan()
    );
    println!(
        "    - {}",
        style("crates/baml-rt-builder/src/optional_tool_bundles.rs").cyan()
    );
    println!(
        "  See the {} tool for an example of runtime initialization.",
        style("memory").cyan()
    );

    Ok(())
}

/// Patch a file using the given patcher.
fn patch_file<P: Patcher>(
    writer: &mut TransactionalWriter,
    workspace_root: &Path,
    patcher: &P,
    tool_name: &str,
) -> Result<()> {
    let path = patcher.file_path(workspace_root);
    let content = fs::read_to_string(&path).map_err(|e| {
        anyhow!(
            "Error: failed to read patch target {}\nCause: {e}\nHint: ensure the workspace is complete and the file exists.",
            path.display()
        )
    })?;

    if patcher.tool_exists(&content, tool_name) {
        println!(
            "{} {} (already patched)",
            style("Skip").yellow(),
            path.display()
        );
        return Ok(());
    }

    let patched = patcher.patch_for_tool(&content, tool_name).map_err(|e| {
        anyhow!(
            "Error: failed to patch {}\nCause: {e}\nHint: this usually means the file format drifted from the expected template.\nNext step: inspect this file and run `cargo agent-platform doctor`.",
            path.display()
        )
    })?;

    writer.stage_edit(&path, patched).map_err(|e| {
        anyhow!(
            "Error: failed to stage edit for {}\nCause: {e}\nHint: verify write permissions and disk availability.",
            path.display()
        )
    })?;
    Ok(())
}

/// Validate tool name is kebab-case and doesn't conflict with existing names.
fn validate_tool_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!(
            "Error: tool name cannot be empty.\nHint: use kebab-case like `github` or `jira-sync`."
        );
    }

    // Must be kebab-case
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!(
            "Error: invalid tool name '{}'.\nHint: use kebab-case with lowercase letters, numbers, and hyphens only (example: `github-sync`).",
            name
        );
    }

    if name.starts_with('-') || name.ends_with('-') {
        bail!(
            "Error: invalid tool name '{}'.\nHint: name cannot start or end with a hyphen.",
            name
        );
    }

    if name.contains("--") {
        bail!(
            "Error: invalid tool name '{}'.\nHint: name cannot contain consecutive hyphens.",
            name
        );
    }

    // Reserved names
    let reserved = ["test", "lib", "bin", "build", "dev"];
    if reserved.contains(&name) {
        bail!(
            "Error: tool name '{}' is reserved.\nHint: choose a different name (examples: `github`, `linear-sync`).",
            name
        );
    }

    Ok(())
}

/// Validate bundle type.
fn validate_bundle(bundle: &str) -> Result<()> {
    if bundle != "support" {
        bail!(
            "Error: unsupported bundle '{}'.\nHint: only `support` is supported in this CLI.\nNext step: rerun with `--bundle support`.",
            bundle
        );
    }
    Ok(())
}

/// Validate access level.
fn validate_access(access: &str) -> Result<()> {
    match access {
        "read" | "write" => Ok(()),
        _ => bail!(
            "Error: unsupported access level '{}'.\nHint: valid values are `read` (default) or `write`.",
            access
        ),
    }
}

/// Find the workspace root by looking for Cargo.toml with [workspace].
fn find_workspace_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;

    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = fs::read_to_string(&cargo_toml)?;
            if content.contains("[workspace]") {
                return Ok(dir);
            }
        }

        if !dir.pop() {
            bail!(
                "Error: could not find workspace root (no Cargo.toml with [workspace] found).\nHint: run this command from the repository root or a subdirectory inside it."
            );
        }
    }
}

/// Capitalize the first letter of a string.
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

    #[test]
    fn test_validate_tool_name() {
        assert!(validate_tool_name("github").is_ok());
        assert!(validate_tool_name("my-tool").is_ok());
        assert!(validate_tool_name("tool123").is_ok());

        assert!(validate_tool_name("").is_err());
        assert!(validate_tool_name("MyTool").is_err());
        assert!(validate_tool_name("my_tool").is_err());
        assert!(validate_tool_name("-tool").is_err());
        assert!(validate_tool_name("tool-").is_err());
        assert!(validate_tool_name("my--tool").is_err());
        assert!(validate_tool_name("test").is_err()); // reserved
    }

    #[test]
    fn test_validate_bundle() {
        assert!(validate_bundle("support").is_ok());
        assert!(validate_bundle("custom").is_err());
    }

    #[test]
    fn test_validate_access() {
        assert!(validate_access("read").is_ok());
        assert!(validate_access("write").is_ok());
        assert!(validate_access("delete").is_err()); // delete no longer valid
        assert!(validate_access("admin").is_err());
    }
}
