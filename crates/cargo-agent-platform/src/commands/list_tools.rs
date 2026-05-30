// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `list-tools` subcommand implementation.
//!
//! Lists all registered tools with current workspace-aware metadata.

use console::style;

use crate::{text::truncate_for_display, tool_catalog::load_cli_tools};

pub fn run() -> anyhow::Result<()> {
    let tools = load_cli_tools()?;
    let total = tools.len();

    if tools.is_empty() {
        println!("{}", style("No tools found.").yellow());
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
        let name = tool.id;
        let description = truncate_for_display(&tool.description, 48);
        let tags = format!("[{}]", tool.tags.join(", "));
        let access = tool.access;

        println!("{:<30} {:<50} {:<25} {}", name, description, tags, access);
    }

    println!();
    println!("{} {} tool(s) registered", style("Total:").bold(), total);

    Ok(())
}
