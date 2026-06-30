// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Stable agent-wide tool / operation vocabulary rendered directly from compiled IR.
//!
//! The rendered text is written to [`CATALOG_SIDECAR_FILE`] inside `baml_src` and loaded by the
//! runtime into `ctx.tags['tool_schema_prelude']`. Unlike the old synthetic-union renderer, this
//! surface contains only stable per-agent tool and operation definitions, never phase-specific
//! return unions.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use baml_rt_tools::{
    schema_allows_empty_or_optional_open_input,
    tools::{ToolCapability, ToolFunctionMetadata},
};
use baml_types::{
    EvaluationContext, LiteralValue,
    ir_type::{TypeNonStreaming, UnionTypeViewGeneric},
};
use internal_baml_core::ir::{ir_hasher::IRSignature, repr::IntermediateRepr};

use crate::builder::baml_gen::escape::escape_baml_description;

/// Historical synthetic catalog function name retained for rewrite-policy compatibility.
pub const CATALOG_FUNCTION_NAME: &str = "AgentToolSchemaCatalog__bamlrt";

/// Sidecar text file holding the rendered catalog. Sits next to `_baml_runtime.baml` inside
/// `baml_src/` so it ships with the agent package and is cluster-deterministic.
pub const CATALOG_SIDECAR_FILE: &str = "_baml_tool_schema_catalog.txt";

/// IR-derived stable vocabulary loaded into `ctx.tags['tool_schema_prelude']`.
#[derive(Debug, Default, Clone)]
pub struct CatalogPlan {
    /// Named IR types included in the rendered sidecar, sorted and deduplicated.
    pub type_names: Vec<String>,
    /// Final rendered stable vocabulary text.
    pub rendered_text: String,
}

#[derive(Debug, Default, Clone)]
struct CatalogDescriptions {
    field_descriptions: BTreeMap<String, BTreeMap<String, String>>,
}

impl CatalogPlan {
    pub fn is_empty(&self) -> bool {
        self.rendered_text.trim().is_empty()
    }
}

/// Compute the stable catalog from compiled IR plus manifest tool metadata.
pub fn collect_catalog_types(
    ir_sig: &IRSignature,
    ir: &IntermediateRepr,
    tool_metadata: &[ToolFunctionMetadata],
    _unified_roots: &baml_rt_tools::UnifiedStepExecutorFunctionsMap,
) -> CatalogPlan {
    if tool_metadata.is_empty() {
        return CatalogPlan::default();
    }

    let mut type_names: BTreeSet<String> = BTreeSet::new();
    for shared in [
        "ArchiveSearchReadInput",
        "ArchivePageReadInput",
        "ArchiveSearchReadStep",
        "ArchivePageReadStep",
        "ReadOnlyFinishStep",
        "StructuredReply",
        "ReplyPart",
        "TextPart",
        "DataPart",
        "ReplyMediaType",
    ] {
        collect_named_type(shared, ir_sig, &mut type_names);
    }

    let mut sorted_tools: Vec<&ToolFunctionMetadata> = tool_metadata.iter().collect();
    sorted_tools.sort_by_key(|tool| tool.name.to_string());
    for tool in &sorted_tools {
        for step in catalog_operation_type_names(tool) {
            collect_named_type(&step, ir_sig, &mut type_names);
        }
        if has_open_input(tool) {
            collect_named_type(&tool.open_input_type.name, ir_sig, &mut type_names);
        }
        collect_named_type(&tool.input_type.name, ir_sig, &mut type_names);
    }

    let type_names: Vec<String> = type_names.into_iter().collect();
    let descriptions = collect_catalog_descriptions(ir);
    let rendered_text = render_catalog_text(ir_sig, &sorted_tools, &type_names, &descriptions);
    CatalogPlan {
        type_names,
        rendered_text,
    }
}

fn render_catalog_text(
    ir_sig: &IRSignature,
    tool_metadata: &[&ToolFunctionMetadata],
    type_names: &[String],
    descriptions: &CatalogDescriptions,
) -> String {
    if tool_metadata.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("Generated from compiled BAML IR.\n");
    out.push_str(
        "Tool metadata selects availability only; field shapes and nested types come from IR.\n\n",
    );
    out.push_str("Available tool types for this agent:\n\n");
    for tool in tool_metadata {
        out.push_str("tool ");
        out.push_str(&tool.name.to_string());
        out.push('\n');
        out.push_str("  purpose: ");
        out.push_str(&single_line_description(&tool.description));
        out.push('\n');
        out.push_str("  open: ");
        out.push_str(if has_open_input(tool) {
            tool.open_input_type.name.as_str()
        } else {
            "None"
        });
        out.push('\n');
        out.push_str("  send: ");
        out.push_str(&tool.input_type.name);
        out.push('\n');
        out.push_str("  capability: ");
        out.push_str(&format!("{:?}", tool.capability));
        out.push('\n');
        out.push_str("  invocation_mode: ");
        out.push_str(baml_rt_tools::capability_invocation_mode(tool.capability));
        out.push('\n');
        out.push_str("  operations: ");
        out.push_str(&catalog_operation_type_names(tool).join(" | "));
        out.push_str("\n\n");
    }

    out.push_str("Type definitions:\n\n");
    for name in type_names {
        if let Some(definition) = render_named_type(name, ir_sig, descriptions) {
            out.push_str(&definition);
            out.push_str("\n\n");
        }
    }
    out.trim_end().to_string()
}

fn catalog_operation_type_names(tool: &ToolFunctionMetadata) -> Vec<String> {
    let class_name = &tool.class_name;
    if tool.capability == ToolCapability::OneShot {
        return ["SendStep", "SearchReadStep", "PageReadStep"]
            .into_iter()
            .map(|suffix| format!("{class_name}{suffix}"))
            .collect();
    }

    [
        "OpenStep",
        "SendStep",
        "SearchReadStep",
        "PageReadStep",
        "FinishStep",
        "AbortStep",
    ]
    .into_iter()
    .map(|suffix| format!("{class_name}{suffix}"))
    .collect()
}

fn has_open_input(tool: &ToolFunctionMetadata) -> bool {
    let name = tool.open_input_type.name.as_str();
    if matches!(name, "()" | "null" | "void") {
        return false;
    }
    !schema_allows_empty_or_optional_open_input(&tool.open_input_schema)
        || tool
            .open_input_schema
            .get("properties")
            .and_then(|value| value.as_object())
            .map(|props| !props.is_empty())
            .unwrap_or(false)
}

fn single_line_description(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_catalog_descriptions(ir: &IntermediateRepr) -> CatalogDescriptions {
    let empty_env: HashMap<String, String> = HashMap::new();
    let eval_ctx = EvaluationContext::new(&empty_env, true);
    let mut descriptions = CatalogDescriptions::default();

    for class_walker in ir.walk_classes() {
        let class_name = class_walker.name().to_string();
        for field_walker in class_walker.walk_fields() {
            let Ok(Some(description)) = field_walker.description(&eval_ctx) else {
                continue;
            };
            let description = single_line_description(&description);
            if description.is_empty() {
                continue;
            }
            descriptions
                .field_descriptions
                .entry(class_name.clone())
                .or_default()
                .insert(field_walker.elem().name.clone(), description);
        }
    }

    descriptions
}

fn collect_named_type(name: &str, ir_sig: &IRSignature, names: &mut BTreeSet<String>) {
    if !names.insert(name.to_string()) {
        return;
    }
    if let Some((_, class_details)) = ir_sig.classes.get(name) {
        for (_, field_ty) in class_details.fields.iter() {
            collect_type_dependencies(field_ty.as_ref(), ir_sig, names);
        }
    } else if let Some(alias) = ir_sig.type_aliases.get(name) {
        collect_type_dependencies(alias.field_type.as_ref(), ir_sig, names);
    }
}

fn collect_type_dependencies(
    ty: &TypeNonStreaming,
    ir_sig: &IRSignature,
    names: &mut BTreeSet<String>,
) {
    match ty {
        TypeNonStreaming::Class { name, .. }
        | TypeNonStreaming::Enum { name, .. }
        | TypeNonStreaming::RecursiveTypeAlias { name, .. } => {
            collect_named_type(name, ir_sig, names)
        }
        TypeNonStreaming::List(inner, _) => collect_type_dependencies(inner, ir_sig, names),
        TypeNonStreaming::Map(key, value, _) => {
            collect_type_dependencies(key, ir_sig, names);
            collect_type_dependencies(value, ir_sig, names);
        }
        TypeNonStreaming::Union(union, _) => match union.view() {
            UnionTypeViewGeneric::Null => {}
            UnionTypeViewGeneric::Optional(inner) => {
                collect_type_dependencies(inner, ir_sig, names)
            }
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => {
                for variant in variants {
                    collect_type_dependencies(variant, ir_sig, names);
                }
            }
        },
        TypeNonStreaming::Tuple(items, _) => {
            for item in items {
                collect_type_dependencies(item, ir_sig, names);
            }
        }
        TypeNonStreaming::Primitive(..)
        | TypeNonStreaming::Literal(..)
        | TypeNonStreaming::Arrow(..)
        | TypeNonStreaming::Top(..) => {}
    }
}

fn render_named_type(
    name: &str,
    ir_sig: &IRSignature,
    descriptions: &CatalogDescriptions,
) -> Option<String> {
    if let Some((_, class_details)) = ir_sig.classes.get(name) {
        let mut out = format!("type {name} = {{\n");
        let class_field_descriptions = descriptions.field_descriptions.get(name);
        for (field_name, field_ty) in class_details.fields.iter() {
            let (field_type, optional) = render_field_type(field_ty.as_ref());
            out.push_str("  ");
            out.push_str(field_name);
            if optional {
                out.push('?');
            }
            out.push_str(": ");
            out.push_str(&field_type);
            if let Some(description) =
                class_field_descriptions.and_then(|fields| fields.get(field_name))
            {
                out.push(' ');
                out.push_str(&render_description_annotation(description));
            }
            out.push('\n');
        }
        out.push('}');
        return Some(out);
    }
    if let Some((_, enum_details)) = ir_sig.enums.get(name) {
        let values = enum_details
            .values
            .iter()
            .map(|value| format!("{value:?}"))
            .collect::<Vec<_>>()
            .join(" | ");
        return Some(format!("type {name} = {values}"));
    }
    ir_sig.type_aliases.get(name).map(|alias| {
        format!(
            "type {name} = {}",
            render_type_expr(alias.field_type.as_ref())
        )
    })
}

fn render_description_annotation(description: &str) -> String {
    format!("@description(\"{}\")", escape_baml_description(description))
}

fn render_field_type(ty: &TypeNonStreaming) -> (String, bool) {
    match ty {
        TypeNonStreaming::Union(union, _) => match union.view() {
            UnionTypeViewGeneric::Optional(inner) => (render_type_expr(inner), true),
            UnionTypeViewGeneric::OneOfOptional(variants) => {
                (render_union_variants(variants.iter().copied()), true)
            }
            _ => (render_type_expr(ty), false),
        },
        _ => (render_type_expr(ty), false),
    }
}

fn render_union_variants<'a>(variants: impl IntoIterator<Item = &'a TypeNonStreaming>) -> String {
    let rendered = variants
        .into_iter()
        .map(render_type_expr)
        .collect::<Vec<_>>()
        .join(" | ");
    format!("({rendered})")
}

fn render_type_expr(ty: &TypeNonStreaming) -> String {
    match ty {
        TypeNonStreaming::Primitive(value, _) => value.basename().to_string(),
        TypeNonStreaming::Class { name, .. }
        | TypeNonStreaming::Enum { name, .. }
        | TypeNonStreaming::RecursiveTypeAlias { name, .. } => name.clone(),
        TypeNonStreaming::Literal(value, _) => match value {
            LiteralValue::String(value) => format!("{value:?}"),
            LiteralValue::Int(value) => value.to_string(),
            LiteralValue::Bool(value) => value.to_string(),
        },
        TypeNonStreaming::List(inner, _) => format!("{}[]", render_type_expr(inner)),
        TypeNonStreaming::Map(key, value, _) => {
            format!(
                "map<{}, {}>",
                render_type_expr(key),
                render_type_expr(value)
            )
        }
        TypeNonStreaming::Union(union, _) => match union.view() {
            UnionTypeViewGeneric::Null => "null".to_string(),
            UnionTypeViewGeneric::Optional(inner) => format!("{}?", render_type_expr(inner)),
            UnionTypeViewGeneric::OneOf(variants) => {
                render_union_variants(variants.iter().copied())
            }
            UnionTypeViewGeneric::OneOfOptional(variants) => {
                format!("{}?", render_union_variants(variants.iter().copied()))
            }
        },
        TypeNonStreaming::Tuple(items, _) => {
            let rendered = items
                .iter()
                .map(render_type_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("tuple<{rendered}>")
        }
        TypeNonStreaming::Arrow(..) | TypeNonStreaming::Top(..) => "string".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_tool(class_name: &str, capability: ToolCapability) -> ToolFunctionMetadata {
        ToolFunctionMetadata {
            name: baml_rt_tools::ToolName::parse("support/sample").expect("valid tool name"),
            class_name: class_name.to_string(),
            description: "sample".to_string(),
            open_input_schema: json!({}),
            input_schema: json!({}),
            output_schema: json!({}),
            open_input_type: baml_rt_tools::tools::ToolTypeSpec {
                name: "()".to_string(),
                ts_decl: None,
            },
            input_type: baml_rt_tools::tools::ToolTypeSpec {
                name: "SupportSampleInput".to_string(),
                ts_decl: None,
            },
            output_type: baml_rt_tools::tools::ToolTypeSpec {
                name: "SupportSampleOutput".to_string(),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: Vec::new(),
            access: None,
            tags: Vec::new(),
            secret_requests: Vec::new(),
            config: None,
            config_bundle: None,
            origin: baml_rt_tools::ToolOrigin::Host,
            backend: baml_rt_tools::ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: baml_rt_tools::SessionPolicy::default(),
            capability,
            event_sources: Vec::new(),
            coordination_baml: None,
        }
    }

    #[test]
    fn empty_plan_reports_empty() {
        let plan = CatalogPlan {
            type_names: Vec::new(),
            rendered_text: String::new(),
        };
        assert!(plan.is_empty());
    }

    #[test]
    fn one_shot_catalog_operations_hide_explicit_lifecycle_steps() {
        let tool = sample_tool("SupportCalculate", ToolCapability::OneShot);
        let operations = catalog_operation_type_names(&tool);

        assert_eq!(
            operations,
            vec![
                "SupportCalculateSendStep".to_string(),
                "SupportCalculateSearchReadStep".to_string(),
                "SupportCalculatePageReadStep".to_string(),
            ]
        );
        assert!(!operations.iter().any(|op| op.ends_with("OpenStep")));
        assert!(!operations.iter().any(|op| op.ends_with("FinishStep")));
        assert!(!operations.iter().any(|op| op.ends_with("AbortStep")));
    }

    #[test]
    fn streaming_catalog_operations_keep_explicit_lifecycle_steps() {
        let tool = sample_tool("SupportStream", ToolCapability::Streaming);
        let operations = catalog_operation_type_names(&tool);

        assert!(operations.contains(&"SupportStreamOpenStep".to_string()));
        assert!(operations.contains(&"SupportStreamFinishStep".to_string()));
        assert!(operations.contains(&"SupportStreamAbortStep".to_string()));
    }
}
