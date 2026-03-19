//! `list-tools` subcommand implementation.
//!
//! Lists all registered tools from the inventory with their metadata.

use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use console::style;

pub fn run() -> anyhow::Result<()> {
    let catalog = InventoryCatalog::new();
    let mut tools: Vec<_> = catalog.iter().collect();

    // Sort by tool name for consistent output
    tools.sort_by_key(|t| t.name.to_string());

    if tools.is_empty() {
        println!("{}", style("No tools found in inventory.").yellow());
        return Ok(());
    }

    // Print header
    println!(
        "{:<30} {:<50} {:<25} {}",
        style("NAME").bold().underlined(),
        style("DESCRIPTION").bold().underlined(),
        style("TAGS").bold().underlined(),
        style("ACCESS").bold().underlined()
    );

    // Print each tool
    for tool in tools {
        let name = tool.name.to_string();
        let description = truncate(&tool.description, 48);
        let tags = format!("[{}]", tool.tags.join(", "));
        let access = tool
            .access
            .as_ref()
            .map(|a| format!("{:?}", a))
            .unwrap_or_else(|| "None".to_string());

        println!("{:<30} {:<50} {:<25} {}", name, description, tags, access);
    }

    println!();
    println!(
        "{} {} tool(s) registered",
        style("Total:").bold(),
        catalog.iter().count()
    );

    Ok(())
}

/// Truncate a string to a maximum length, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}
