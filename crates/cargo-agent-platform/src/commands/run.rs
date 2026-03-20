//! `run` subcommand — run one or more agent packages.
//!
//! This is a thin wrapper around `baml-agent-runner` that validates packages exist
//! before running and provides helpful error messages.

use std::{path::Path, process::Command};

use anyhow::{Result, bail};
use console::style;

/// Run the `run` command.
///
/// Validates that all package files exist, then passes through to baml-agent-runner.
pub fn run(packages: &[String], extra_args: &[String]) -> Result<()> {
    if packages.is_empty() {
        bail!(
            "No agent packages specified.\n\n\
             Usage: cargo agent-platform run <package.tar.gz>... [options]\n\n\
             Example:\n\
             \x20 cargo agent-platform run clickup-agent-1.0.0.tar.gz --serve-http 127.0.0.1:8080\n\n\
             To build an agent first:\n\
             \x20 cargo agent-platform build clickup-agent"
        );
    }

    // Validate all packages exist
    let mut missing = Vec::new();
    for pkg in packages {
        if !Path::new(pkg).exists() {
            missing.push(pkg.as_str());
        }
    }

    if !missing.is_empty() {
        let missing_list = missing
            .iter()
            .map(|p| format!("  - {}", p))
            .collect::<Vec<_>>()
            .join("\n");

        // Try to extract agent names from missing packages for helpful hints
        let hints: Vec<String> = missing
            .iter()
            .filter_map(|p| extract_agent_name(p))
            .map(|name| format!("  cargo agent-platform build {}", name))
            .collect();

        let hint_text = if hints.is_empty() {
            String::new()
        } else {
            format!("\n\nTo build the missing package(s):\n{}", hints.join("\n"))
        };

        bail!(
            "Package(s) not found:\n{}{}\n\n\
             Make sure the .tar.gz files exist in the specified path.",
            missing_list,
            hint_text
        );
    }

    // Build the command
    let runner_bin = find_runner_binary()?;

    println!(
        "{} Running {} agent package(s)...",
        style("▶").green().bold(),
        packages.len()
    );
    for pkg in packages {
        println!("  {}", style(pkg).cyan());
    }
    if !extra_args.is_empty() {
        println!("  Options: {}", style(extra_args.join(" ")).dim());
    }
    println!();

    // Execute baml-agent-runner with all args
    let mut cmd = Command::new(&runner_bin);
    cmd.args(packages);
    cmd.args(extra_args);

    // Run and wait for completion
    let status = cmd.status().map_err(|e| {
        anyhow::anyhow!(
            "Failed to execute baml-agent-runner: {}\n\
             Binary path: {}",
            e,
            runner_bin
        )
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("baml-agent-runner exited with code {}", code);
    }

    Ok(())
}

/// Try to extract agent name from a package filename.
/// e.g., "clickup-agent-1.0.0.tar.gz" -> "clickup-agent"
fn extract_agent_name(filename: &str) -> Option<String> {
    let name = Path::new(filename).file_name()?.to_str()?;

    // Remove .tar.gz suffix
    let without_ext = name.strip_suffix(".tar.gz")?;

    // Try to find version pattern and remove it
    // Pattern: name-X.Y.Z where X, Y, Z are numbers
    if let Some(pos) = without_ext.rfind('-') {
        let (name_part, version_part) = without_ext.split_at(pos);
        let version = &version_part[1..]; // skip the '-'

        // Check if it looks like a version (starts with digit)
        if version
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            return Some(name_part.to_string());
        }
    }

    // If no version pattern found, return the whole name without extension
    Some(without_ext.to_string())
}

/// Find the baml-agent-runner binary.
/// First checks if it's in PATH, then looks in target/debug and target/release.
fn find_runner_binary() -> Result<String> {
    // Check if in PATH
    if Command::new("baml-agent-runner")
        .arg("--help")
        .output()
        .is_ok()
    {
        return Ok("baml-agent-runner".to_string());
    }

    // Look in target directories relative to workspace root
    let workspace_root = find_workspace_root()?;

    // Prefer release build if it exists
    let release_path = workspace_root.join("target/release/baml-agent-runner");
    if release_path.exists() {
        return Ok(release_path.to_string_lossy().to_string());
    }

    let debug_path = workspace_root.join("target/debug/baml-agent-runner");
    if debug_path.exists() {
        return Ok(debug_path.to_string_lossy().to_string());
    }

    bail!(
        "baml-agent-runner not found.\n\n\
         Build it first with:\n\
         \x20 cargo build -p baml-agent-runner\n\n\
         Or for release build:\n\
         \x20 cargo build -p baml-agent-runner --release"
    );
}

/// Find the workspace root by looking for Cargo.toml with [workspace].
fn find_workspace_root() -> Result<std::path::PathBuf> {
    let mut current = std::env::current_dir()?;
    loop {
        let cargo_toml = current.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = std::fs::read_to_string(&cargo_toml)?;
            if content.contains("[workspace]") {
                return Ok(current);
            }
        }
        if !current.pop() {
            bail!("Could not find workspace root");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_agent_name() {
        assert_eq!(
            extract_agent_name("clickup-agent-1.0.0.tar.gz"),
            Some("clickup-agent".to_string())
        );
        assert_eq!(
            extract_agent_name("my-cool-agent-2.3.4.tar.gz"),
            Some("my-cool-agent".to_string())
        );
        assert_eq!(
            extract_agent_name("agent.tar.gz"),
            Some("agent".to_string())
        );
        assert_eq!(
            extract_agent_name("/path/to/clickup-agent-1.0.0.tar.gz"),
            Some("clickup-agent".to_string())
        );
        assert_eq!(extract_agent_name("not-a-tarball.txt"), None);
    }
}
