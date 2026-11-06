// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Boundary and parity evidence for the canonical tool schema graph.

use std::{collections::HashMap, ops::Deref};

use baml_rt_builder::builder::{
    baml_gen::render_baml_tool_interfaces_from_metadata,
    tool_schema_graph::{
        OpenInputContract, SchemaDeclarationKind, SchemaOwner, SchemaTypeRef, ToolSchemaGraph,
        baml::render_tool_schema_baml,
    },
};
use baml_rt_tools::{
    OpaqueJson, ToolBackend, ToolOrigin, tool_catalog::resolve_manifest_tools,
    tool_schema::json_schema_value, tools::ToolName,
};
use baml_tools_system::metadata::system_callback_metadata;
use serde_json::json;

// ---------------------------------------------------------------------------
// Graph boundary: normalized projection + rendered BAML.
// ---------------------------------------------------------------------------

#[test]
fn tool_schema_graph_representative_projection_snapshot() {
    let content_block = json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string" },
            "text": { "type": "string" }
        },
        "required": ["kind", "text"],
        "additionalProperties": false
    });
    let opaque_json = json_schema_value::<OpaqueJson>();
    let representative_schema = json!({
        "type": "object",
        "properties": {
            "request": { "$ref": "#/$defs/NestedRequest" },
            "required_nullable": { "type": ["string", "null"] },
            "optional_note": { "type": "string" },
            "labels": { "type": "object", "additionalProperties": { "type": "string" } },
            "addresses": { "type": "array", "items": { "$ref": "#/$defs/Address" } },
            "content": {
                "oneOf": [
                    content_block.clone(),
                    { "type": "array", "items": content_block }
                ]
            },
            "opaque_payload": opaque_json
        },
        "required": ["request", "required_nullable", "labels", "addresses", "opaque_payload"],
        "$defs": {
            "Address": {
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "lines": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["city", "lines"]
            },
            "NestedRequest": {
                "type": "object",
                "properties": {
                    "address": { "$ref": "#/$defs/Address" },
                    "kind": { "$ref": "#/$defs/RequestKind" }
                },
                "required": ["address", "kind"]
            },
            "RequestKind": { "type": "string", "enum": ["create", "update"] }
        }
    });

    let mut tool = system_callback_metadata();
    tool.name = ToolName::parse("graph/representative").expect("tool name");
    tool.class_name = "GraphRepresentative".to_string();
    tool.input_type.name = "RepresentativeToolInput".to_string();
    tool.input_schema = representative_schema;
    tool.output_type.name = "RepresentativeToolOutput".to_string();
    tool.output_schema = json!({
        "type": "object",
        "properties": { "accepted": { "type": "boolean" } },
        "required": ["accepted"],
        "additionalProperties": false
    });
    let graph = ToolSchemaGraph::from_tools(&[tool]).expect("representative schema should lower");
    let rendered = render_tool_schema_baml(&graph).expect("render BAML");

    insta::assert_snapshot!(
        "tool_schema_graph_representative_projection",
        format!(
            "== graph projection ==\n{}\n== rendered BAML ==\n{}",
            project_graph(&graph),
            rendered.trim_end()
        )
    );
}

// ---------------------------------------------------------------------------
// Static / external parity: origin and backend must not change the graph.
// ---------------------------------------------------------------------------

#[test]
fn tool_schema_graph_static_external_parity_matrix() {
    let mut tool_names = vec!["support/calculate"];
    #[cfg(feature = "notion")]
    tool_names.push("support/notion");
    #[cfg(feature = "slack")]
    tool_names.push("support/slack");

    for tool_name in tool_names {
        let static_tool = resolve_manifest_tools(&[tool_name.to_string()])
            .unwrap_or_else(|error| panic!("resolve {tool_name}: {error}"))
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{tool_name} present"));
        let static_graph = ToolSchemaGraph::from_tools(std::slice::from_ref(&static_tool))
            .unwrap_or_else(|error| panic!("lower static {tool_name}: {error}"));

        let local_name = tool_name.split('/').next_back().expect("local tool name");
        let mut external_tool = static_tool.clone();
        external_tool.name =
            ToolName::parse(&format!("external/{local_name}")).expect("parse external name");
        external_tool.class_name = format!("External{local_name}");
        external_tool.origin = ToolOrigin::Guest;
        external_tool.backend = ToolBackend::External;
        external_tool.baml_decl = None;

        let external_graph = ToolSchemaGraph::from_tools(&[external_tool])
            .unwrap_or_else(|error| panic!("lower external equivalent of {tool_name}: {error}"));

        let static_render = render_tool_schema_baml(&static_graph)
            .unwrap_or_else(|error| panic!("render static {tool_name}: {error}"));
        let external_render = render_tool_schema_baml(&external_graph)
            .unwrap_or_else(|error| panic!("render external equivalent of {tool_name}: {error}"));
        assert_eq!(
            static_render, external_render,
            "{tool_name}: equivalent schemas must render identically regardless of origin"
        );

        let static_shapes = declaration_shapes(&static_graph);
        let external_shapes = declaration_shapes(&external_graph);
        assert_eq!(
            static_shapes, external_shapes,
            "{tool_name}: equivalent schemas must have equivalent normalized declarations"
        );
        assert!(!static_shapes.is_empty(), "{tool_name}: declaration set");
    }
}

// ---------------------------------------------------------------------------
// Production renderer: resolved metadata plus one graph, never `baml_decl`.
// ---------------------------------------------------------------------------

#[test]
fn production_tool_interfaces_use_the_graph_not_static_baml_declarations() {
    let mut tool = resolve_manifest_tools(&["support/calculate".to_string()])
        .expect("resolve calculator")
        .into_iter()
        .next()
        .expect("calculator present");
    tool.baml_decl = Some("class MustNotAppearInGeneratedBaml {}".to_string());

    let graph = ToolSchemaGraph::from_tools(std::slice::from_ref(&tool)).expect("lower graph");
    let generated = render_baml_tool_interfaces_from_metadata(&[tool], &graph)
        .expect("render production tool interfaces");

    assert!(
        generated.contains("class CalculatorInput"),
        "graph-generated BAML:\n{generated}"
    );
    assert!(
        generated.contains("input CalculatorInput"),
        "graph-generated BAML:\n{generated}"
    );
    assert!(
        !generated.contains("MustNotAppearInGeneratedBaml"),
        "production rendering must ignore compatibility baml_decl metadata"
    );
}

// ---------------------------------------------------------------------------
// Dual-render: compile `baml_decl` and graph-rendered BAML, compare wire-level IR.
// ---------------------------------------------------------------------------

#[test]
fn tool_schema_graph_dual_render_normalized_ir_matrix() {
    let mut tool_names = vec!["support/calculate"];
    #[cfg(feature = "notion")]
    tool_names.push("support/notion");
    #[cfg(feature = "slack")]
    tool_names.push("support/slack");

    for tool_name in tool_names {
        let tool = resolve_manifest_tools(&[tool_name.to_string()])
            .unwrap_or_else(|error| panic!("resolve {tool_name}: {error}"))
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{tool_name} present"));
        let baml_decl = tool
            .baml_decl
            .clone()
            .unwrap_or_else(|| panic!("{tool_name} ships a pre-rendered baml_decl"));

        let graph = ToolSchemaGraph::from_tools(&[tool])
            .unwrap_or_else(|error| panic!("lower {tool_name}: {error}"));
        let graph_baml = render_tool_schema_baml(&graph)
            .unwrap_or_else(|error| panic!("render graph BAML for {tool_name}: {error}"));

        let existing_runtime = compile_dto("existing.baml", &baml_decl);
        let graph_runtime = compile_dto("graph.baml", &graph_baml);
        let mut existing_ir = normalized_dto_ir(&existing_runtime);
        let mut graph_ir = normalized_dto_ir(&graph_runtime);

        // Notion and Slack mark collection outputs `#[serde(default)]`: omission is accepted on
        // the wire. Their legacy hand-rendered BAML made those fields required, while the
        // canonical schemas correctly make them optional. Remove only those recorded legacy
        // deltas; every other compiled IR signature must match.
        if tool_name == "support/notion" {
            remove_expected_signature(
                &mut existing_ir,
                "class NotionOutput[blocks:NotionBlockSummary[];has_more:bool;message:string;next_cursor:string?;pages:NotionPageSummary[];sources:NotionSource[]]",
                tool_name,
            );
            remove_expected_signature(
                &mut graph_ir,
                "class NotionOutput[blocks:NotionBlockSummary[]?;has_more:bool;message:string;next_cursor:string?;pages:NotionPageSummary[]?;sources:NotionSource[]?]",
                tool_name,
            );
        }
        if tool_name == "support/slack" {
            remove_expected_signature(
                &mut existing_ir,
                "class SlackOutput[conversations:SlackConversationSummary[];has_more:bool;message:string;messages:SlackMessageSummary[];next_cursor:string?;sources:SlackSource[];users:SlackUserSummary[]]",
                tool_name,
            );
            remove_expected_signature(
                &mut graph_ir,
                "class SlackOutput[conversations:SlackConversationSummary[]?;has_more:bool;message:string;messages:SlackMessageSummary[]?;next_cursor:string?;sources:SlackSource[]?;users:SlackUserSummary[]?]",
                tool_name,
            );
        }

        assert_eq!(
            existing_ir, graph_ir,
            "{tool_name}: graph rendering must preserve normalized compiled DTO IR except recorded schema corrections"
        );

        if tool_name == "support/calculate" {
            assert!(
                graph_baml.contains("@alias(\"+\")") && graph_baml.contains("@alias(\"/\")"),
                "symbolic enum wire values must survive the schema path:\n{graph_baml}"
            );
            assert!(
                graph_baml.contains("Left-hand integer operand."),
                "field descriptions must survive the schema path:\n{graph_baml}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compile_dto(name: &str, baml: &str) -> baml_runtime::BamlRuntime {
    let files = HashMap::from([(name.to_string(), baml.to_string())]);
    baml_runtime::BamlRuntime::from_file_content(
        "test",
        &files,
        HashMap::<String, String>::new(),
        internal_baml_core::feature_flags::FeatureFlags::default(),
    )
    .expect("compile DTO declarations")
}

fn type_str(ty: &baml_types::TypeIR) -> String {
    use baml_types::ir_type::{TypeGeneric, UnionTypeViewGeneric};
    match ty {
        TypeGeneric::Top(_) => "any".to_string(),
        TypeGeneric::Primitive(tv, _) => tv.basename().to_string(),
        TypeGeneric::Class { name, .. }
        | TypeGeneric::Enum { name, .. }
        | TypeGeneric::RecursiveTypeAlias { name, .. } => name.clone(),
        TypeGeneric::Union(union, _) => {
            let (mut parts, optional) = match union.view() {
                UnionTypeViewGeneric::Null => return "null".to_string(),
                UnionTypeViewGeneric::Optional(inner) => {
                    return format!("{}?", type_str(inner));
                }
                UnionTypeViewGeneric::OneOf(variants) => (
                    variants
                        .iter()
                        .map(|variant| type_str(variant))
                        .collect::<Vec<_>>(),
                    false,
                ),
                UnionTypeViewGeneric::OneOfOptional(variants) => (
                    variants
                        .iter()
                        .map(|variant| type_str(variant))
                        .collect::<Vec<_>>(),
                    true,
                ),
            };
            if optional {
                parts.push("null".to_string());
            }
            parts.sort();
            format!("({})", parts.join(" | "))
        }
        TypeGeneric::List(item, _) => format!("{}[]", type_str(item)),
        TypeGeneric::Map(key, value, _) => {
            format!("map<{}, {}>", type_str(key), type_str(value))
        }
        TypeGeneric::Tuple(items, _) => format!(
            "({})",
            items.iter().map(type_str).collect::<Vec<_>>().join(", ")
        ),
        TypeGeneric::Literal(lv, _) => {
            use baml_types::LiteralValue;
            match lv {
                LiteralValue::String(s) => format!("\"{s}\""),
                LiteralValue::Int(i) => i.to_string(),
                LiteralValue::Bool(b) => b.to_string(),
            }
        }
        TypeGeneric::Arrow(_, _) => "<function>".to_string(),
    }
}

/// Normalized projection of compiled BAML DTO IR. Field and enum identifiers are resolved to
/// wire spellings, union members are sorted, and every representable IR type keeps its shape.
fn normalized_dto_ir(runtime: &baml_runtime::BamlRuntime) -> Vec<String> {
    let ir = runtime.ir.deref();
    let mut lines = Vec::new();

    for class in &ir.classes {
        let mut fields: Vec<String> = class
            .elem
            .static_fields
            .iter()
            .map(|f| {
                let wire_name = f
                    .attributes
                    .alias()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| f.elem.name.clone());
                format!("{wire_name}:{}", type_str(&f.elem.r#type.elem))
            })
            .collect();
        fields.sort();
        lines.push(format!("class {}[{}]", class.elem.name, fields.join(";")));
    }

    for en in &ir.enums {
        let mut values: Vec<String> = en
            .elem
            .values
            .iter()
            .map(|(v, _)| {
                v.attributes
                    .alias()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| v.elem.0.clone())
            })
            .collect();
        values.sort();
        lines.push(format!("enum {}[{}]", en.elem.name, values.join(";")));
    }

    for alias in &ir.type_aliases {
        lines.push(format!(
            "alias {}={}",
            alias.elem.name,
            type_str(&alias.elem.r#type.elem)
        ));
    }

    lines.sort();
    lines
}

fn remove_expected_signature(signatures: &mut Vec<String>, expected: &str, tool_name: &str) {
    let index = signatures
        .iter()
        .position(|signature| signature == expected)
        .unwrap_or_else(|| panic!("{tool_name}: missing recorded IR signature `{expected}`"));
    signatures.remove(index);
}

fn declaration_shapes(graph: &ToolSchemaGraph) -> Vec<(String, String)> {
    let mut shapes: Vec<(String, String)> = graph
        .symbols
        .iter()
        .map(|(symbol, id)| (symbol.clone(), graph.declarations[id].fingerprint.clone()))
        .collect();
    shapes.sort();
    shapes
}

fn project_graph(graph: &ToolSchemaGraph) -> String {
    let mut decls: Vec<_> = graph.declarations.values().collect();
    decls.sort_by_key(|d| d.symbol.clone());

    let mut out = String::new();
    for decl in decls {
        out.push_str(&format!(
            "{} ({}) fp={}\n",
            decl.symbol,
            decl.id.as_u32(),
            decl.fingerprint
        ));
        for owner in &decl.owners {
            out.push_str(&format!("  owner {}\n", owner_disp(owner)));
        }
        match &decl.kind {
            SchemaDeclarationKind::Object(obj) => {
                for field in &obj.fields {
                    out.push_str(&format!(
                        "  field {}: {} required={}\n",
                        field.wire_name,
                        ty_disp(graph, &field.ty),
                        field.required
                    ));
                }
            }
            SchemaDeclarationKind::Enum(e) => {
                for variant in &e.variants {
                    out.push_str(&format!(
                        "  variant {} = {:?}\n",
                        variant.preferred_identifier, variant.wire_value
                    ));
                }
            }
            SchemaDeclarationKind::Alias(ty) => {
                out.push_str(&format!("  alias {}\n", ty_disp(graph, ty)));
            }
        }
        if !decl.dependencies.is_empty() {
            let deps: Vec<String> = decl
                .dependencies
                .iter()
                .map(|id| graph.declarations[id].symbol.clone())
                .collect();
            out.push_str(&format!("  deps [{}]\n", deps.join(", ")));
        }
        out.push('\n');
    }

    // Per-tool contracts, when present.
    for (name, contract) in &graph.tools {
        out.push_str(&format!("tool {name} (class {})\n", contract.class_name));
        match &contract.open_input {
            OpenInputContract::Absent => out.push_str("  open_input: <absent>\n"),
            OpenInputContract::Present { ty, requirement } => {
                out.push_str(&format!(
                    "  open_input: {} ({requirement:?})\n",
                    ty_disp(graph, ty)
                ));
            }
        }
        out.push_str(&format!("  input: {}\n", ty_disp(graph, &contract.input)));
        out.push_str(&format!("  output: {}\n", ty_disp(graph, &contract.output)));
    }

    out
}

fn owner_disp(owner: &SchemaOwner) -> String {
    match owner {
        SchemaOwner::PublicRoot { tool_name, role } => {
            format!("public {tool_name} {}", role.as_str())
        }
        SchemaOwner::StandaloneRoot { schema_name } => {
            format!("standalone {schema_name}")
        }
        SchemaOwner::Nested {
            document,
            tool_name,
            role,
            path,
        } => format!(
            "nested {document} {} {} /{}",
            tool_name.as_deref().unwrap_or("<standalone>"),
            role.map(|value| value.as_str()).unwrap_or("<none>"),
            path.join("/")
        ),
        SchemaOwner::SharedDefinition {
            document,
            tool_name,
            role,
            original_name,
            json_pointer,
        } => format!(
            "definition {document} {} {} {original_name} {json_pointer}",
            tool_name.as_deref().unwrap_or("<standalone>"),
            role.map(|value| value.as_str()).unwrap_or("<none>"),
        ),
    }
}

fn ty_disp(graph: &ToolSchemaGraph, ty: &SchemaTypeRef) -> String {
    match ty {
        SchemaTypeRef::String => "string".to_string(),
        SchemaTypeRef::Integer => "int".to_string(),
        SchemaTypeRef::Number => "float".to_string(),
        SchemaTypeRef::Boolean => "bool".to_string(),
        SchemaTypeRef::Null => "null".to_string(),
        SchemaTypeRef::JsonValue => "json".to_string(),
        SchemaTypeRef::Literal(l) => format!("literal({l:?})"),
        SchemaTypeRef::Array(inner) => format!("{}[]", ty_disp(graph, inner)),
        SchemaTypeRef::Map(inner) => format!("map<string, {}>", ty_disp(graph, inner)),
        SchemaTypeRef::Named(id) => graph
            .declarations
            .get(id)
            .map(|d| d.symbol.clone())
            .unwrap_or_else(|| format!("<unresolved {}>", id.as_u32())),
        SchemaTypeRef::Union(arms) => arms
            .iter()
            .map(|a| ty_disp(graph, a))
            .collect::<Vec<_>>()
            .join(" | "),
        SchemaTypeRef::VecOrOne(inner) => {
            format!("({} | {}[])", ty_disp(graph, inner), ty_disp(graph, inner))
        }
    }
}
