//! `list-event-sources` subcommand implementation.
//!
//! Lists all event source kinds declared by tools in the inventory,
//! plus known schema versions.

use std::collections::BTreeMap;

use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use console::style;

use crate::text::truncate_for_display;

/// Known schema versions for event delivery.
///
/// NOTE: Hardcoded for now. In the future, this should come from the same
/// producer inventory used by the runner.
const KNOWN_SCHEMA_VERSIONS: &[(&str, &str)] = &[
    (
        "host.source-records.v1",
        "Generic raw source-ingress batch produced by host-managed event sources",
    ),
    (
        "system.callback.v1",
        "Durable host-native callback event emitted by system/callback",
    ),
    (
        "task-daemon.interpretation.v1",
        "Task daemon event interpretation (Slack, ClickUp, GitHub Issues)",
    ),
];

pub fn run() -> anyhow::Result<()> {
    let catalog = InventoryCatalog::new();

    // Collect event sources from all tools
    // Map: source_kind -> Vec<(tool_name, tool_description)>
    let mut source_kinds: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    for tool in catalog.iter() {
        for source in &tool.event_sources {
            source_kinds
                .entry(source.as_str().to_string())
                .or_default()
                .push((tool.name.to_string(), tool.description.clone()));
        }
    }

    // Print event source kinds
    println!();
    println!(
        "{}",
        style("Event Source Kinds (declared by tools):")
            .bold()
            .underlined()
    );
    println!();

    if source_kinds.is_empty() {
        println!(
            "  {}",
            style("No tools currently declare event_sources.").dim()
        );
        println!();
        println!(
            "  {}",
            style("Hint: Tools declare event sources via #[baml_tool(..., event_sources = [\"kind\"])]").dim()
        );
    } else {
        println!(
            "  {:<20} {:<35} {}",
            style("SOURCE KIND").bold(),
            style("TOOL").bold(),
            style("DESCRIPTION").bold()
        );

        for (source_kind, tools) in &source_kinds {
            for (i, (tool_name, tool_desc)) in tools.iter().enumerate() {
                let kind_display = if i == 0 {
                    source_kind.clone()
                } else {
                    String::new()
                };
                println!(
                    "  {:<20} {:<35} {}",
                    style(&kind_display).green(),
                    tool_name,
                    truncate_for_display(tool_desc, 40)
                );
            }
        }
    }

    // Print known schema versions
    println!();
    println!("{}", style("Known Schema Versions:").bold().underlined());
    println!();
    println!(
        "  {:<40} {}",
        style("SCHEMA VERSION").bold(),
        style("DESCRIPTION").bold()
    );

    for (schema, desc) in KNOWN_SCHEMA_VERSIONS {
        println!("  {:<40} {}", style(*schema).cyan(), desc);
    }

    println!();
    println!(
        "{}",
        style(
            "Note: Agents subscribe to source kinds (e.g. \"system/callback\") in manifest subscriptions."
        )
        .dim()
    );
    println!(
        "{}",
        style(
            "      Schema versions identify the envelope format of events produced by each source."
        )
        .dim()
    );
    println!();

    // Print summary
    let total_sources = source_kinds.len();
    let total_schemas = KNOWN_SCHEMA_VERSIONS.len();
    println!(
        "{} {} event source kind(s), {} known schema version(s)",
        style("Total:").bold(),
        total_sources,
        total_schemas
    );

    Ok(())
}
