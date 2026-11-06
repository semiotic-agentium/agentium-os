// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Template fragments merged into [`super::GENERATED_BAML_PRELUDE_FILE`] (prelude + per-tool cards).

use std::path::Path;

use baml_rt_tools::{
    StaticToolSnapshotCatalog,
    external_tools::{
        build_builder_catalog, build_builder_catalog_with_mcp_root,
        build_builder_catalog_with_roots, build_builder_catalog_with_static_catalog,
    },
    tool_catalog::resolve_manifest_tools_with_catalog,
    tools::ToolFunctionMetadata,
};
#[cfg(feature = "dev-tools")]
use baml_tools_calculator as _;

use super::{
    escape::escape_baml_description, prelude::generated_tools_prelude, prompt_copy,
    writer::BamlWriter,
};
use crate::builder::{
    error::{BamlBuilderError, Result},
    tool_schema_graph::{
        OpenInputContract, ToolSchemaContract, ToolSchemaGraph,
        baml::{render_schema_type_ref, render_tool_schema_baml},
    },
};

/// Generate BAML tool interfaces from already-resolved metadata and its canonical schema graph.
///
/// This is the production entry point. It does not resolve a catalog, inspect raw JSON Schema, or
/// read compatibility declarations such as `ToolFunctionMetadata::baml_decl`.
pub fn render_baml_tool_interfaces_from_metadata(
    tools: &[ToolFunctionMetadata],
    graph: &ToolSchemaGraph,
) -> Result<String> {
    let mut w = BamlWriter::new();
    w.push_block(&generated_tools_prelude());
    let out = w.as_mut_string();

    let domain_types = render_tool_schema_baml(graph)?;
    if !domain_types.is_empty() {
        out.push_str("// Domain types generated from the canonical tool schema graph\n");
        out.push_str(&domain_types);
        if !domain_types.ends_with('\n') {
            out.push('\n');
        }
    }

    for tool in tools {
        let contract = graph.tools.get(&tool.name.to_string()).ok_or_else(|| {
            BamlBuilderError::InvalidArgument(format!(
                "tool schema graph has no contract for resolved tool `{}`",
                tool.name
            ))
        })?;
        generate_tool_card_baml(out, tool, contract, graph)?;
        out.push('\n');
        generate_tool_baml_interface(out, tool, contract, graph)?;
        out.push('\n');
    }

    Ok(w.into_string())
}

/// Compatibility entry point for callers that still start with manifest tool names.
///
/// The build pipeline resolves tools once in `WorkspaceReady` and calls
/// [`render_baml_tool_interfaces_from_metadata`] directly. Keep this wrapper for tests and
/// non-pipeline callers until they can provide resolved metadata.
pub fn render_baml_tool_interfaces(tool_names: &[String]) -> Result<String> {
    render_baml_tool_interfaces_with_mcp_root(tool_names, None)
}

pub fn render_baml_tool_interfaces_with_mcp_root(
    tool_names: &[String],
    mcp_root: Option<&Path>,
) -> Result<String> {
    render_baml_tool_interfaces_with_roots(tool_names, mcp_root, None)
}

pub fn render_baml_tool_interfaces_with_roots(
    tool_names: &[String],
    mcp_root: Option<&Path>,
    external_cache_root: Option<&Path>,
) -> Result<String> {
    render_baml_tool_interfaces_with_static_catalog(tool_names, mcp_root, external_cache_root, None)
}

pub fn render_baml_tool_interfaces_with_static_catalog(
    tool_names: &[String],
    mcp_root: Option<&Path>,
    external_cache_root: Option<&Path>,
    static_catalog: Option<StaticToolSnapshotCatalog>,
) -> Result<String> {
    // Force link so inventory sees these metadata registrations (regen_fixtures + builder).
    #[cfg(feature = "dev-tools")]
    let _ = baml_tools_calculator::support_calculate_metadata;
    let _ = baml_tools_system::metadata::system_internal_a2a_metadata;
    #[cfg(feature = "clickup")]
    let _ = baml_tools_clickup::ClickUpTool::new;
    #[cfg(feature = "notion")]
    let _ = baml_tools_notion::NotionTool::new;
    #[cfg(feature = "slack")]
    let _ = baml_tools_slack::SlackTool::new;

    let builder_catalog = match static_catalog {
        Some(catalog) => build_builder_catalog_with_static_catalog(
            mcp_root,
            external_cache_root,
            Some(Box::new(catalog)),
        )?,
        None => match (mcp_root, external_cache_root) {
            (_, Some(_)) => build_builder_catalog_with_roots(mcp_root, external_cache_root)?,
            (Some(root), None) => build_builder_catalog_with_mcp_root(Some(root))?,
            (None, None) => build_builder_catalog()?,
        },
    };
    let tools = resolve_manifest_tools_with_catalog(&builder_catalog, tool_names)?;
    let graph = ToolSchemaGraph::from_tools(&tools)?;
    render_baml_tool_interfaces_from_metadata(&tools, &graph)
}

fn generate_tool_card_baml(
    output: &mut String,
    tool: &ToolFunctionMetadata,
    contract: &ToolSchemaContract,
    graph: &ToolSchemaGraph,
) -> Result<()> {
    use crate::builder::error::write_line;

    let class_name = &tool.class_name;
    let card_name = format!("{class_name}ToolCard");
    let tool_name = tool.name.to_string();
    let description = escape_baml_description(&tool.description);
    let policy = format!("{:?}", tool.session_policy);
    let capability = format!("{:?}", tool.capability);
    let invocation_mode = baml_rt_tools::capability_invocation_mode(tool.capability);
    let input_summary = render_schema_type_ref(graph, &contract.input);

    write_line(
        output,
        &format!("/// Tool card for {tool_name}: metadata for polymorphic Open selection."),
    )?;
    write_line(output, &format!("class {card_name} {{"))?;
    write_line(output, &format!("  tool_name \"{tool_name}\""))?;
    write_line(output, &format!("  description \"{description}\""))?;
    write_line(output, &format!("  session_policy \"{policy}\""))?;
    write_line(output, &format!("  capability \"{capability}\""))?;
    write_line(output, &format!("  invocation_mode \"{invocation_mode}\""))?;
    write_line(
        output,
        &format!(
            "  input_summary \"{}\"",
            escape_baml_description(&input_summary)
        ),
    )?;
    if !tool.tags.is_empty() {
        let tags_desc = tool.tags.join(", ");
        write_line(
            output,
            &format!("  tags string[] @description(\"Tool tags: {tags_desc}\")"),
        )?;
    }
    write_line(output, "}")?;
    Ok(())
}

fn generate_tool_baml_interface(
    output: &mut String,
    tool: &ToolFunctionMetadata,
    contract: &ToolSchemaContract,
    graph: &ToolSchemaGraph,
) -> Result<()> {
    use crate::builder::error::write_line;

    let class_name = &tool.class_name;
    let input_type = render_schema_type_ref(graph, &contract.input);

    let open_step_name = format!("{class_name}OpenStep");
    let send_step_name = format!("{class_name}SendStep");
    let search_read_step_name = format!("{class_name}SearchReadStep");
    let page_read_step_name = format!("{class_name}PageReadStep");
    let finish_step_name = format!("{class_name}FinishStep");
    let abort_step_name = format!("{class_name}AbortStep");
    let step_union_name = format!("{class_name}SessionStep");
    let plan_type_name = format!("{class_name}SessionPlan");
    let access_note = tool
        .access
        .map(|access| format!(" Access: {access}."))
        .unwrap_or_default();

    let tool_name = tool.name.to_string();
    let is_a2a = tool_name.contains("/a2a") || tool_name.contains("_a2a");

    let send_input_desc = if is_a2a {
        "Conversational message payload. Set text to a non-empty string. Never null, never omit text."
    } else {
        "Payload for this step."
    };
    let step_desc = if is_a2a {
        prompt_copy::STEP_DESC_A2A
    } else {
        prompt_copy::STEP_DESC_DEFAULT
    };

    write_line(output, &format!("class {open_step_name} {{"))?;
    write_line(output, "  op \"Open\"")?;
    write_line(
        output,
        &format!("  tool_name \"{tool_name}\" @description(\"Tool to open\")"),
    )?;
    if let OpenInputContract::Present { ty, requirement } = &contract.open_input {
        let open_type = render_schema_type_ref(graph, ty);
        let optional_suffix = if matches!(
            requirement,
            crate::builder::tool_schema_graph::OpenInputRequirement::Optional
        ) {
            "?"
        } else {
            ""
        };
        let initial_input_desc = if optional_suffix.is_empty() {
            "Required open payload."
        } else {
            "Optional open payload."
        };
        write_line(
            output,
            &format!(
                "  initial_input {open_type}{optional_suffix} @description(\"{initial_input_desc}\")"
            ),
        )?;
    }
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {send_step_name} {{"))?;
    write_line(output, "  op \"Send\"")?;
    write_line(
        output,
        &format!("  tool_name \"{tool_name}\" @description(\"Tool this Send targets\")"),
    )?;
    write_line(
        output,
        &format!("  input {input_type} @description(\"{send_input_desc}\")"),
    )?;
    write_line(
        output,
        &format!(
            "  citations string[] @description(\"{}\")",
            escape_baml_description(prompt_copy::CITATIONS_SEND_STEP)
        ),
    )?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {search_read_step_name} {{"))?;
    write_line(output, "  op \"SearchRead\"")?;
    write_line(
        output,
        &format!(
            "  input ArchiveSearchReadInput @description(\"{}\")",
            escape_baml_description(prompt_copy::SEARCH_READ_STEP_INPUT_DESCRIPTION)
        ),
    )?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {page_read_step_name} {{"))?;
    write_line(output, "  op \"PageRead\"")?;
    write_line(
        output,
        &format!(
            "  input ArchivePageReadInput @description(\"{}\")",
            escape_baml_description(prompt_copy::PAGE_READ_STEP_INPUT_DESCRIPTION)
        ),
    )?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {finish_step_name} {{"))?;
    write_line(output, "  op \"Finish\"")?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {abort_step_name} {{"))?;
    write_line(output, "  op \"Abort\"")?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(
        output,
        &format!(
            "type {step_union_name} = {open_step_name} | {send_step_name} | {search_read_step_name} | {page_read_step_name} | {finish_step_name} | {abort_step_name}",
        ),
    )?;
    write_line(output, "")?;

    write_line(output, &format!("class {plan_type_name} {{"))?;
    write_line(
        output,
        &format!("  step {step_union_name} @description(\"{step_desc}{access_note}\")",),
    )?;
    write_line(
        output,
        &format!(
            "  citations string[] @description(\"{}\")",
            escape_baml_description(prompt_copy::CITATIONS_DECISION_OR_SYNTHESIS)
        ),
    )?;
    write_line(output, "}")?;

    Ok(())
}
