// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Boundary contract for the JSON Schema to BAML compatibility path.

use std::collections::HashMap;

use baml_rt_builder::builder::schema_to_baml::generate_baml_types_from_schemas;
use baml_rt_tools::{OpaqueJson, tool_schema::json_schema_value};
use serde_json::json;

#[test]
fn schema_to_baml_representative_boundary_snapshot() {
    // The tagged content block and opaque JSON schemas keep the corpus compact while covering
    // the other forms accepted by this compatibility API.
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
            "labels": {
                "type": "object",
                "additionalProperties": { "type": "string" }
            },
            "addresses": {
                "type": "array",
                "items": { "$ref": "#/$defs/Address" }
            },
            "content": {
                "oneOf": [
                    content_block.clone(),
                    { "type": "array", "items": content_block }
                ]
            },
            "opaque_payload": opaque_json
        },
        "required": [
            "request",
            "required_nullable",
            "labels",
            "addresses",
            "opaque_payload"
        ],
        "$defs": {
            "Address": {
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "lines": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
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
            "RequestKind": {
                "type": "string",
                "enum": ["create", "update"]
            }
        }
    });

    let schemas = HashMap::from([("RepresentativeToolInput".to_string(), representative_schema)]);
    let type_names = HashMap::from([(
        "RepresentativeToolInput".to_string(),
        "RepresentativeToolInput".to_string(),
    )]);
    let rendered = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("representative schemas should use the current compatibility path");

    let unsupported_schemas = HashMap::from([(
        "UnsupportedAllOf".to_string(),
        json!({
            "allOf": [
                { "type": "string" },
                { "minLength": 1 }
            ]
        }),
    )]);
    let unsupported_type_names = HashMap::from([(
        "UnsupportedAllOf".to_string(),
        "UnsupportedAllOf".to_string(),
    )]);
    let diagnostic =
        generate_baml_types_from_schemas(&unsupported_schemas, &unsupported_type_names)
            .expect_err("allOf must remain unsupported by the compatibility path")
            .to_string();

    insta::assert_snapshot!(
        "schema_to_baml_representative_boundary",
        format!(
            "== rendered BAML ==\n{}\n== unsupported allOf diagnostic ==\n{}\n",
            rendered.trim_end(),
            diagnostic,
        )
    );
}
