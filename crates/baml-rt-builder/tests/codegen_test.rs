//! Unit tests for BAML and TypeScript code generation
//!
//! Uses insta snapshots to verify generated output, reducing round trips
//! during development and catching regressions.

use baml_rt_builder::builder::{
    baml_gen::render_baml_tool_interfaces, schema_to_baml::generate_baml_types_from_schemas,
    ts_gen::render_ts_declarations,
};
use serde_json::Value;
use std::collections::HashMap;

#[test]
fn test_baml_tool_interface_generation() {
    let tool_names = vec!["support/calculate".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate BAML tool interfaces");

    insta::assert_snapshot!("baml_tool_interfaces", baml_output);
}

#[test]
fn test_baml_tool_interface_with_multiple_tools() {
    // Test with multiple tools (if we had more)
    let tool_names = vec!["support/calculate".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate BAML tool interfaces");

    insta::assert_snapshot!("baml_tool_interfaces_multiple", baml_output);
}

#[test]
fn test_typescript_declaration_generation() {
    let tool_names = vec!["support/calculate".to_string()];
    let function_names = vec!["ChooseRiteTool".to_string()];

    let ts_output = render_ts_declarations(&function_names, &tool_names)
        .expect("Should generate TypeScript declarations");

    insta::assert_snapshot!("typescript_declarations", ts_output);
}

#[test]
fn test_schema_to_baml_enum_generation() {
    // Test enum generation from JSON schema
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Create a simple enum schema
    let enum_schema: Value = serde_json::json!({
        "enum": ["Add", "Subtract", "Multiply", "Divide"],
        "type": "string"
    });

    schemas.insert("MathOperation".to_string(), enum_schema);
    type_names.insert("MathOperation".to_string(), "MathOperation".to_string());

    let baml_output =
        generate_baml_types_from_schemas(&schemas, &type_names).expect("Should generate BAML enum");

    insta::assert_snapshot!("baml_enum_from_schema", baml_output);
}

#[test]
fn test_schema_to_baml_class_generation() {
    // Test class generation from JSON schema
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Create a simple class schema
    let class_schema: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "left": {
                "type": "integer",
                "format": "int64"
            },
            "right": {
                "type": "integer",
                "format": "int64"
            },
            "optional_field": {
                "type": "string"
            }
        },
        "required": ["left", "right"]
    });

    schemas.insert("Expression".to_string(), class_schema);
    type_names.insert("Expression".to_string(), "Expression".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class");

    insta::assert_snapshot!("baml_class_from_schema", baml_output);
}

#[test]
fn test_schema_to_baml_nested_types() {
    // Test nested type generation with $ref
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Create nested schemas (simulating $defs structure)
    let expression_schema: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "left": {
                "type": "integer",
                "format": "int64"
            },
            "operation": {
                "$ref": "#/$defs/MathOperation"
            },
            "right": {
                "type": "integer",
                "format": "int64"
            }
        },
        "required": ["left", "operation", "right"],
        "$defs": {
            "MathOperation": {
                "enum": ["Add", "Subtract", "Multiply", "Divide"],
                "type": "string"
            }
        }
    });

    let math_op_schema: Value = serde_json::json!({
        "enum": ["Add", "Subtract", "Multiply", "Divide"],
        "type": "string"
    });

    schemas.insert("Expression".to_string(), expression_schema);
    schemas.insert("MathOperation".to_string(), math_op_schema);
    type_names.insert("Expression".to_string(), "Expression".to_string());
    type_names.insert("MathOperation".to_string(), "MathOperation".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate nested BAML types");

    insta::assert_snapshot!("baml_nested_types_from_schema", baml_output);
}

#[test]
fn test_calculator_tool_metadata_schemas() {
    // Test with actual calculator tool metadata to verify real-world schemas
    use baml_rt_tools::tool_catalog::resolve_manifest_tools;

    let tool_names = vec!["support/calculate".to_string()];
    let tool_metadata = resolve_manifest_tools(&tool_names).expect("Should resolve tool metadata");

    assert_eq!(tool_metadata.len(), 1);
    let tool = &tool_metadata[0];

    // Create a summary string for snapshot
    let metadata_summary = format!(
        "name: {}\nclass_name: {}\ninput_type: {}\noutput_type: {}\nopen_input_type: {}\nhas_input_schema: {}\nhas_output_schema: {}\nhas_open_input_schema: {}",
        tool.name,
        tool.class_name,
        tool.input_type.name,
        tool.output_type.name,
        tool.open_input_type.name,
        tool.input_schema.is_object(),
        tool.output_schema.is_object(),
        tool.open_input_schema.is_object()
    );

    insta::assert_snapshot!("calculator_tool_metadata", metadata_summary);
}

#[test]
fn test_baml_generation_with_unit_open_input() {
    // Test that unit type () for open_input is handled correctly
    let tool_names = vec!["support/calculate".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate BAML tool interfaces");

    // Extract just the OpenStep class to verify unit type handling
    let open_step_start = baml_output.find("class SupportCalculateOpenStep").unwrap();
    let open_step_end = baml_output[open_step_start..]
        .find("class SupportCalculateSendStep")
        .unwrap();
    let open_step_section = &baml_output[open_step_start..open_step_start + open_step_end];

    insta::assert_snapshot!("baml_open_step_unit_input", open_step_section);
}

#[test]
fn test_typescript_generation_includes_tool_functions() {
    let tool_names = vec!["support/calculate".to_string()];
    let function_names = vec!["TestFunction".to_string()];

    let ts_output = render_ts_declarations(&function_names, &tool_names)
        .expect("Should generate TypeScript declarations");

    insta::assert_snapshot!("typescript_tool_functions", ts_output);
}

#[test]
fn test_schema_to_baml_array_types() {
    // Test array type generation - arrays are used within classes, not as standalone types
    // So we test by creating a class with an array property
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    let class_with_array: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "string"
                }
            }
        },
        "required": ["items"]
    });

    schemas.insert("ArrayTest".to_string(), class_with_array);
    type_names.insert("ArrayTest".to_string(), "ArrayTest".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class with array");

    insta::assert_snapshot!("baml_array_types", baml_output);
}

#[test]
fn test_schema_to_baml_description_escaping() {
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    let class_schema: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "plain": {
                "type": "string",
                "description": "A plain description"
            },
            "quoted": {
                "type": "string",
                "description": "Field with \"quoted\" value"
            },
            "backslash": {
                "type": "string",
                "description": "Path like C:\\Users\\test"
            },
            "mixed": {
                "type": "string",
                "description": "She said \"hello\\world\""
            }
        },
        "required": ["plain", "quoted", "backslash", "mixed"]
    });

    schemas.insert("EscapeTest".to_string(), class_schema);
    type_names.insert("EscapeTest".to_string(), "EscapeTest".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class with escaped descriptions");

    insta::assert_snapshot!("baml_description_escaping", baml_output);
}

#[test]
fn test_schema_to_baml_primitive_types() {
    // Test that primitive types are correctly converted
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Test various primitive schemas (not used directly, but tests conversion logic)
    let _int_schema: Value = serde_json::json!({
        "type": "integer",
        "format": "int64"
    });

    let _float_schema: Value = serde_json::json!({
        "type": "number"
    });

    let _string_schema: Value = serde_json::json!({
        "type": "string"
    });

    let _bool_schema: Value = serde_json::json!({
        "type": "boolean"
    });

    // Note: Primitives aren't typically generated as standalone types,
    // but this tests the conversion logic in json_schema_to_baml_type
    // The actual test is that they're used correctly in class properties
    let class_with_primitives: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "int_field": {
                "type": "integer",
                "format": "int64"
            },
            "float_field": {
                "type": "number"
            },
            "string_field": {
                "type": "string"
            },
            "bool_field": {
                "type": "boolean"
            }
        },
        "required": ["int_field", "float_field", "string_field", "bool_field"]
    });

    schemas.insert("PrimitiveTest".to_string(), class_with_primitives);
    type_names.insert("PrimitiveTest".to_string(), "PrimitiveTest".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class with primitives");

    insta::assert_snapshot!("baml_primitives_in_class", baml_output);
}
