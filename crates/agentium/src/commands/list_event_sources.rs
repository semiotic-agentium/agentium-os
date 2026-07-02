// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `list-event-sources` subcommand implementation.
//!
//! Lists all event source kinds declared by tools in the runner/static catalog,
//! compatibility source kinds, and known schema versions.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::Path,
};

use anyhow::{Context, Result};
use baml_rt_tools::ToolCatalog;
use console::style;

use crate::{
    event_schemas::{KNOWN_COMPATIBILITY_SOURCE_KINDS, KNOWN_EVENT_SCHEMAS},
    text::truncate_for_display,
};

pub fn run(repository_url: &str, snapshot_cache: Option<&str>) -> Result<()> {
    let source_kinds = match snapshot_cache {
        Some(root) => {
            let catalog =
                baml_rt_builder::static_tool_registry::load_static_tool_catalog_from_cache(
                    Path::new(root),
                )
                .with_context(|| {
                    format!(
                        "loading static tool catalog from {}",
                        baml_rt_builder::static_tool_registry::static_tool_catalog_path(root)
                            .display()
                    )
                })?;
            collect_tool_declared_source_kinds(&catalog)
        }
        None => {
            let rt = tokio::runtime::Runtime::new()?;
            let catalog = rt.block_on(async {
                baml_rt_builder::static_tool_registry::fetch_static_tool_catalog(repository_url)
                    .await
                    .with_context(|| {
                        format!(
                            "loading static tool catalog from runner/repository at {}",
                            repository_url.trim_end_matches('/')
                        )
                    })
            })?;
            collect_tool_declared_source_kinds(&catalog)
        }
    };

    print!("{}", render_report(&source_kinds));

    Ok(())
}

type ToolDeclaredSourceKinds = BTreeMap<String, Vec<(String, String)>>;

fn collect_tool_declared_source_kinds<C: ToolCatalog + ?Sized>(
    catalog: &C,
) -> ToolDeclaredSourceKinds {
    let mut source_kinds: ToolDeclaredSourceKinds = BTreeMap::new();

    for tool in catalog.iter() {
        for source in &tool.event_sources {
            source_kinds
                .entry(source.as_str().to_string())
                .or_default()
                .push((tool.name.to_string(), tool.description.clone()));
        }
    }

    source_kinds
}

fn render_report(source_kinds: &ToolDeclaredSourceKinds) -> String {
    let mut out = String::new();

    writeln!(&mut out).expect("write");
    writeln!(
        &mut out,
        "{}",
        style("Event Source Kinds (declared by tools):")
            .bold()
            .underlined()
    )
    .expect("write");
    writeln!(&mut out).expect("write");

    if source_kinds.is_empty() {
        writeln!(
            &mut out,
            "  {}",
            style("No tools currently declare event_sources.").dim()
        )
        .expect("write");
        writeln!(&mut out).expect("write");
        writeln!(
            &mut out,
            "  {}",
            style("Hint: Tools declare event sources via #[baml_tool(..., event_sources = [\"kind\"])]").dim()
        )
        .expect("write");
    } else {
        writeln!(
            &mut out,
            "  {:<20} {:<35} {}",
            style("SOURCE KIND").bold(),
            style("TOOL").bold(),
            style("DESCRIPTION").bold()
        )
        .expect("write");

        for (source_kind, tools) in source_kinds {
            for (i, (tool_name, tool_desc)) in tools.iter().enumerate() {
                let kind_display = if i == 0 {
                    source_kind.clone()
                } else {
                    String::new()
                };
                writeln!(
                    &mut out,
                    "  {:<20} {:<35} {}",
                    style(&kind_display).green(),
                    tool_name,
                    truncate_for_display(tool_desc, 40)
                )
                .expect("write");
            }
        }
    }

    writeln!(&mut out).expect("write");
    writeln!(
        &mut out,
        "{}",
        style("Compatibility Source Kinds (task-daemon bridge):")
            .bold()
            .underlined()
    )
    .expect("write");
    writeln!(&mut out).expect("write");
    writeln!(
        &mut out,
        "  {:<20} {}",
        style("SOURCE KIND").bold(),
        style("DESCRIPTION").bold()
    )
    .expect("write");

    for source in KNOWN_COMPATIBILITY_SOURCE_KINDS {
        writeln!(
            &mut out,
            "  {:<20} {}",
            style(source.kind).yellow(),
            source.description
        )
        .expect("write");
    }

    writeln!(&mut out).expect("write");
    writeln!(
        &mut out,
        "{}",
        style("Known Schema Versions:").bold().underlined()
    )
    .expect("write");
    writeln!(&mut out).expect("write");
    writeln!(
        &mut out,
        "  {:<40} {}",
        style("SCHEMA VERSION").bold(),
        style("DESCRIPTION").bold()
    )
    .expect("write");

    for schema in KNOWN_EVENT_SCHEMAS {
        writeln!(
            &mut out,
            "  {:<40} {}",
            style(schema.version).cyan(),
            schema.description
        )
        .expect("write");
    }

    writeln!(&mut out).expect("write");
    writeln!(
        &mut out,
        "{}",
        style(
            "Note: Agents subscribe to source kinds (e.g. \"system/callback\") in manifest subscriptions."
        )
        .dim()
    )
    .expect("write");
    writeln!(
        &mut out,
        "{}",
        style("      Task-daemon publishes host.source-records.v1 via POST /events/publish.").dim()
    )
    .expect("write");
    writeln!(
        &mut out,
        "{}",
        style(
            "      Schema versions identify the envelope format of events produced by each source."
        )
        .dim()
    )
    .expect("write");
    writeln!(&mut out).expect("write");

    let total_sources = total_source_kind_count(source_kinds);
    let total_schemas = KNOWN_EVENT_SCHEMAS.len();
    writeln!(
        &mut out,
        "{} {} event source kind(s), {} known schema version(s)",
        style("Total:").bold(),
        total_sources,
        total_schemas
    )
    .expect("write");

    out
}

fn total_source_kind_count(source_kinds: &ToolDeclaredSourceKinds) -> usize {
    source_kinds
        .keys()
        .map(String::as_str)
        .chain(
            KNOWN_COMPATIBILITY_SOURCE_KINDS
                .iter()
                .map(|source| source.kind),
        )
        .collect::<BTreeSet<_>>()
        .len()
}

#[cfg(test)]
mod tests {
    use super::{ToolDeclaredSourceKinds, render_report, total_source_kind_count};

    #[test]
    fn render_report_includes_task_daemon_compatibility_source_kinds() {
        let source_kinds = ToolDeclaredSourceKinds::from([(
            "slack".to_string(),
            vec![(
                "support/slack".to_string(),
                "Read-only Slack integration for conversation history".to_string(),
            )],
        )]);

        let report = render_report(&source_kinds);

        assert!(report.contains("Compatibility Source Kinds (task-daemon bridge):"));
        assert!(report.contains("clickup"));
        assert!(report.contains("github_issues"));
        assert!(report.contains("host.source-records.v1"));
        assert!(report.contains("POST /events/publish"));
    }

    #[test]
    fn total_source_kind_count_unions_tool_declared_and_compatibility_source_kinds() {
        let source_kinds = ToolDeclaredSourceKinds::from([
            (
                "slack".to_string(),
                vec![("support/slack".to_string(), "Slack".to_string())],
            ),
            (
                "system/callback".to_string(),
                vec![("system/callback".to_string(), "Callback".to_string())],
            ),
        ]);

        assert_eq!(total_source_kind_count(&source_kinds), 4);
    }
}
