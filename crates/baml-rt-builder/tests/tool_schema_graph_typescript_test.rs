// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Boundary evidence for the graph-to-TypeScript renderer.
//!
//! One authoritative test covers the representative graph snapshot, the type-expression matrix,
//! and origin parity, because all three are views of the same renderer contract.

use std::collections::HashMap;

use baml_rt_builder::builder::tool_schema_graph::{
    ToolSchemaGraph,
    typescript::{ToolTypescriptSurface, render_tool_typescript_surface},
};
use baml_rt_tools::{
    OpaqueJson, ToolBackend, ToolOrigin, tool_schema::json_schema_value, tools::ToolName,
};
use baml_tools_system::metadata::system_callback_metadata;
use serde_json::{Value, json};

#[test]
fn tool_schema_graph_typescript_surface() {
    let static_tool = representative_tool();
    let static_graph = ToolSchemaGraph::from_tools(std::slice::from_ref(&static_tool))
        .expect("lower representative");
    let surface = render_tool_typescript_surface(&static_graph).expect("render TypeScript surface");

    // Boundary snapshot: declarations, dependencies, and per-tool root expressions.
    insta::assert_snapshot!(
        "tool_schema_graph_typescript_surface",
        project_surface(&surface)
    );

    // Expression matrix: requiredness, nullability, literals, arrays, maps, and VecOrOne.
    assert_expression_matrix();

    // BAML identifiers with hyphens and TypeScript keywords must become valid, unique symbols.
    assert_typescript_symbol_shaping();

    // Origin parity: equivalent graphs render equivalent TypeScript after root normalization.
    let mut external_tool = static_tool.clone();
    external_tool.name = ToolName::parse("graph/external-representative").expect("external name");
    external_tool.class_name = "ExternalGraphRepresentative".to_string();
    external_tool.input_type.name = "ExternalRepresentativeToolInput".to_string();
    external_tool.output_type.name = "ExternalRepresentativeToolOutput".to_string();
    external_tool.origin = ToolOrigin::Guest;
    external_tool.backend = ToolBackend::External;
    external_tool.baml_decl = None;

    let external_graph =
        ToolSchemaGraph::from_tools(&[external_tool]).expect("lower external representative");
    let external_surface =
        render_tool_typescript_surface(&external_graph).expect("render external surface");

    assert_eq!(
        normalized_declarations(&surface, ""),
        normalized_declarations(&external_surface, "External"),
        "static and external tools with equivalent schemas must render equivalent declarations"
    );
    assert_eq!(
        normalized_contracts(&surface, ""),
        normalized_contracts(&external_surface, "External"),
        "static and external tools with equivalent schemas must render equivalent contracts"
    );
}

fn representative_tool() -> baml_rt_tools::tools::ToolFunctionMetadata {
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
    tool
}

fn assert_expression_matrix() {
    let cases: Vec<(&str, Value, &[&str])> = vec![
        (
            "required, optional, nullable, literals, and quoted keys",
            json!({
                "type": "object",
                "properties": {
                    "required_value": { "type": "string" },
                    "optional_value": { "type": "string" },
                    "required_nullable": { "type": ["string", "null"] },
                    "optional_nullable": { "type": ["string", "null"] },
                    "kind": { "const": "text" },
                    "count": { "const": 3 },
                    "ratio": { "const": 1.5 },
                    "enabled": { "const": true },
                    "1st value": { "type": "string" },
                    "a?b": { "type": "string" }
                },
                "required": [
                    "required_value",
                    "required_nullable",
                    "kind",
                    "count",
                    "ratio",
                    "enabled",
                    "1st value",
                    "a?b"
                ]
            }),
            &[
                "required_value: string;",
                "optional_value?: string;",
                "required_nullable: string | null;",
                "optional_nullable?: string | null;",
                "kind: \"text\";",
                "count: 3;",
                "ratio: 1.5;",
                "enabled: true;",
                "\"1st value\": string;",
                "\"a?b\": string;",
                "[key: string]: JsonValue | undefined;",
            ],
        ),
        (
            "arrays, maps, enums, and vec-or-one",
            json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": { "anyOf": [{ "type": "string" }, { "type": "integer" }] }
                    },
                    "labels": {
                        "type": "object",
                        "additionalProperties": { "type": "string" }
                    },
                    "names": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" } }
                        ]
                    },
                    "mode": { "type": "string", "enum": ["fast", "slow"] },
                    "level": { "type": "integer", "enum": [1, 2] }
                },
                "required": ["items", "labels", "names", "mode", "level"],
                "additionalProperties": false
            }),
            &[
                "items: (string | number)[];",
                "labels: Record<string, string>;",
                "names: string | string[];",
                "export type MatrixRootMode = \"fast\" | \"slow\";",
                "export type MatrixRootLevel = 1 | 2;",
            ],
        ),
        (
            "union and opaque aliases",
            json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] }),
            &["export type MatrixRoot = string | number;"],
        ),
        (
            "opaque JSON alias",
            json!({}),
            &["export type MatrixRoot = JsonValue;"],
        ),
        (
            "closed empty object",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            &["export interface MatrixRoot {\n}"],
        ),
    ];

    for (label, schema, fragments) in cases {
        let graph = ToolSchemaGraph::from_root_schemas(
            &HashMap::from([("MatrixRoot".to_string(), schema)]),
            &HashMap::from([("MatrixRoot".to_string(), "MatrixRoot".to_string())]),
        )
        .unwrap_or_else(|error| panic!("{label}: lower root: {error}"));
        let surface = render_tool_typescript_surface(&graph)
            .unwrap_or_else(|error| panic!("{label}: render surface: {error}"));
        let rendered = surface
            .declarations
            .iter()
            .map(|declaration| declaration.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for fragment in fragments {
            assert!(
                rendered.contains(fragment),
                "{label}: expected `{fragment}` in:\n{rendered}"
            );
        }
        if label == "arrays, maps, enums, and vec-or-one" {
            assert!(
                !rendered.contains("[key: string]"),
                "{label}: forbidden additional properties must not emit an index signature:\n{rendered}"
            );
        }
    }
}

fn assert_typescript_symbol_shaping() {
    let graph = ToolSchemaGraph::from_root_schemas(
        &HashMap::from([(
            "Root".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "hyphenated": { "$ref": "#/$defs/Foo-Bar" },
                    "plain": { "$ref": "#/$defs/Foo_Bar" },
                    "keyword": { "$ref": "#/$defs/class" }
                },
                "required": ["hyphenated", "plain", "keyword"],
                "$defs": {
                    "Foo-Bar": {
                        "type": "object",
                        "properties": { "id": { "type": "string" } },
                        "required": ["id"]
                    },
                    "Foo_Bar": {
                        "type": "object",
                        "properties": { "id": { "type": "integer" } },
                        "required": ["id"]
                    },
                    "class": {
                        "type": "object",
                        "properties": { "id": { "type": "boolean" } },
                        "required": ["id"]
                    }
                }
            }),
        )]),
        &HashMap::from([("Root".to_string(), "Root".to_string())]),
    )
    .expect("lower hyphenated and reserved symbols");
    let surface = render_tool_typescript_surface(&graph).expect("render TypeScript surface");
    let rendered = surface
        .declarations
        .iter()
        .map(|declaration| declaration.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!rendered.contains("Foo-Bar"), "no hyphenated type names");
    assert!(
        rendered.contains("export interface Foo_Bar {"),
        "valid graph symbol is preserved:\n{rendered}"
    );
    assert!(
        rendered.contains("export interface Foo_Bar_2 {"),
        "hyphenated symbol gets a stable suffix:\n{rendered}"
    );
    assert!(
        rendered.contains("export interface class_Type {"),
        "reserved word is mapped:\n{rendered}"
    );
    assert!(rendered.contains("hyphenated: Foo_Bar_2;"));
    assert!(rendered.contains("plain: Foo_Bar;"));
    assert!(rendered.contains("keyword: class_Type;"));
}

fn project_surface(surface: &ToolTypescriptSurface) -> String {
    let mut output = String::new();
    output.push_str("== declarations ==\n");
    for declaration in &surface.declarations {
        let dependencies: Vec<&str> = declaration
            .dependencies
            .iter()
            .map(String::as_str)
            .collect();
        output.push_str(&format!(
            "{} deps=[{}]\n{}\n\n",
            declaration.symbol,
            dependencies.join(", "),
            declaration.body
        ));
    }

    output.push_str("== tools ==\n");
    for contract in surface.tools.values() {
        output.push_str(&format!("{}\n", contract.tool_name));
        match &contract.open_input {
            None => output.push_str("  open_input: <absent>\n"),
            Some(ty) => output.push_str(&format!(
                "  open_input: {ty} ({:?})\n",
                contract
                    .open_requirement
                    .expect("present Open input has a requirement")
            )),
        }
        output.push_str(&format!("  input: {}\n", contract.input));
        output.push_str(&format!("  output: {}\n", contract.output));
    }
    output
}

/// Declaration symbols and bodies with root-name prefixes removed, sorted as a set because
/// TypeScript declarations do not depend on emission order.
fn normalized_declarations(surface: &ToolTypescriptSurface, strip: &str) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = surface
        .declarations
        .iter()
        .map(|declaration| {
            let mut body = declaration.body.clone();
            body.push_str("\ndeps=[");
            let mut dependencies: Vec<String> = declaration
                .dependencies
                .iter()
                .map(|dependency| dependency.replace(strip, ""))
                .collect();
            dependencies.sort();
            body.push_str(&dependencies.join(", "));
            body.push(']');
            (
                declaration.symbol.replace(strip, ""),
                body.replace(strip, ""),
            )
        })
        .collect();
    entries.sort();
    entries
}

/// Tool root expressions without tool names or root-name prefixes.
fn normalized_contracts(surface: &ToolTypescriptSurface, strip: &str) -> Vec<String> {
    let mut contracts: Vec<String> = surface
        .tools
        .values()
        .map(|contract| {
            format!(
                "{:?}|{:?}|{}|{}",
                contract.open_input, contract.open_requirement, contract.input, contract.output
            )
            .replace(strip, "")
        })
        .collect();
    contracts.sort();
    contracts
}
