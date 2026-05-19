// Fixture types used only by the derive macro.
#![allow(dead_code)]

use std::collections::HashMap;

use baml_derive::BamlType;
use baml_derive_core::JsonSchemaType;

// ─── Simple struct → object schema ───────────────────────────────

#[derive(BamlType)]
struct SchSimpleInput {
    pub name: String,
    pub count: i32,
    pub active: bool,
    pub score: f64,
}

#[test]
fn simple_struct_schema() {
    let schema = SchSimpleInput::json_schema_inline();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["name"]["type"], "string");
    assert_eq!(schema["properties"]["count"]["type"], "integer");
    assert_eq!(schema["properties"]["active"]["type"], "boolean");
    assert_eq!(schema["properties"]["score"]["type"], "number");
    // All fields are required (none are Option<T>)
    let req = schema["required"].as_array().expect("required array");
    assert!(req.iter().any(|v| v == "name"));
    assert!(req.iter().any(|v| v == "count"));
}

#[derive(BamlType)]
struct SchFieldDescription {
    #[baml(description = "User-visible plain text content.")]
    pub text: Option<String>,
}

#[test]
fn field_description_is_emitted_in_json_schema() {
    let schema = SchFieldDescription::json_schema_inline();
    assert_eq!(
        schema["properties"]["text"]["description"],
        "User-visible plain text content."
    );
}

// ─── Struct with Option<T> — optional fields not in required ─────

#[derive(BamlType)]
struct SchOptionalFields {
    pub required_name: String,
    pub optional_count: Option<i32>,
    pub optional_tag: Option<String>,
}

#[test]
fn optional_fields_not_required() {
    let schema = SchOptionalFields::json_schema_inline();
    let req = schema["required"].as_array().expect("required array");
    assert!(
        req.iter().any(|v| v == "required_name"),
        "required_name should be required"
    );
    assert!(
        !req.iter().any(|v| v == "optional_count"),
        "optional_count should not be required"
    );
    // Optional field schema: anyOf [integer, null]
    let count_schema = &schema["properties"]["optional_count"];
    let any_of = count_schema["anyOf"].as_array().expect("anyOf array");
    assert_eq!(any_of.len(), 2);
    assert!(any_of.iter().any(|v| v["type"] == "integer"));
    assert!(any_of.iter().any(|v| v["type"] == "null"));
}

// ─── Vec<T> → array schema ────────────────────────────────────────

#[derive(BamlType)]
struct SchVecField {
    pub items: Vec<String>,
    pub counts: Vec<i32>,
}

#[test]
fn vec_field_schema() {
    let schema = SchVecField::json_schema_inline();
    let items_schema = &schema["properties"]["items"];
    assert_eq!(items_schema["type"], "array");
    assert_eq!(items_schema["items"]["type"], "string");
    let counts_schema = &schema["properties"]["counts"];
    assert_eq!(counts_schema["type"], "array");
    assert_eq!(counts_schema["items"]["type"], "integer");
}

// ─── #[baml(vec_or_one)] — one T or T[] on the wire ─────────────────

#[derive(BamlType)]
struct SchVecOrOneOpt {
    #[baml(vec_or_one)]
    pub items: Option<Vec<String>>,
}

#[derive(BamlType)]
struct SchVecOrOneReq {
    #[baml(vec_or_one)]
    pub items: Vec<String>,
}

#[test]
fn vec_or_one_optional_schema() {
    let schema = SchVecOrOneOpt::json_schema_inline();
    let items = &schema["properties"]["items"];
    let any_of = items["anyOf"].as_array().expect("anyOf");
    assert_eq!(any_of.len(), 2);
    let inner = &any_of[0];
    let one_of = inner["oneOf"].as_array().expect("oneOf");
    assert_eq!(one_of.len(), 2);
    assert_eq!(one_of[0]["type"], "string");
    assert_eq!(one_of[1]["type"], "array");
    assert_eq!(one_of[1]["items"]["type"], "string");
    assert_eq!(any_of[1]["type"], "null");
}

#[test]
fn vec_or_one_required_schema() {
    let schema = SchVecOrOneReq::json_schema_inline();
    let items = &schema["properties"]["items"];
    let one_of = items["oneOf"].as_array().expect("oneOf");
    assert_eq!(one_of.len(), 2);
    assert_eq!(one_of[0]["type"], "string");
    assert_eq!(one_of[1]["type"], "array");
    assert_eq!(one_of[1]["items"]["type"], "string");
}

// ─── HashMap<K,V> → additionalProperties schema ───────────────────

#[derive(BamlType)]
struct SchMapField {
    pub metadata: HashMap<String, String>,
}

#[test]
fn map_field_schema() {
    let schema = SchMapField::json_schema_inline();
    let meta_schema = &schema["properties"]["metadata"];
    assert_eq!(meta_schema["type"], "object");
    assert_eq!(meta_schema["additionalProperties"]["type"], "string");
}

// ─── Nested user type ─────────────────────────────────────────────

#[derive(BamlType)]
enum SchPriority {
    Low,
    Medium,
    High,
}

#[derive(BamlType)]
struct SchTaskInput {
    pub title: String,
    pub priority: SchPriority,
}

#[test]
fn nested_user_type_schema() {
    let schema = SchTaskInput::json_schema_inline();
    let priority_schema = &schema["properties"]["priority"];
    // Should delegate to SchPriority::json_schema_inline()
    assert_eq!(priority_schema["type"], "string");
    let enum_vals = priority_schema["enum"].as_array().expect("enum array");
    assert!(enum_vals.iter().any(|v| v == "Low"));
    assert!(enum_vals.iter().any(|v| v == "High"));
}

// ─── #[baml(skip)] removes field from schema ─────────────────────

#[derive(BamlType)]
struct SchSkipField {
    pub public: String,
    #[baml(skip)]
    pub internal: String,
}

#[test]
fn skip_field_absent_in_schema() {
    let schema = SchSkipField::json_schema_inline();
    let props = schema["properties"].as_object().expect("properties object");
    assert!(props.contains_key("public"), "public should be in schema");
    assert!(
        !props.contains_key("internal"),
        "internal should be skipped"
    );
}

// ─── Unit enum → string enum schema ──────────────────────────────

#[derive(BamlType)]
enum SchStatus {
    Open,
    InProgress,
    Closed,
}

#[test]
fn unit_enum_schema() {
    let schema = SchStatus::json_schema_inline();
    assert_eq!(schema["type"], "string");
    let vals = schema["enum"].as_array().expect("enum values");
    assert!(vals.iter().any(|v| v == "Open"));
    assert!(vals.iter().any(|v| v == "InProgress"));
    assert!(vals.iter().any(|v| v == "Closed"));
}

#[test]
fn unit_enum_skip_variant_in_schema() {
    #[derive(BamlType)]
    enum SchStatusSkip {
        Open,
        #[baml(skip)]
        Internal,
        Closed,
    }
    let schema = SchStatusSkip::json_schema_inline();
    let vals = schema["enum"].as_array().expect("enum values");
    assert!(
        !vals.iter().any(|v| v == "Internal"),
        "skipped variant must not appear"
    );
    assert!(vals.iter().any(|v| v == "Open"));
    assert!(vals.iter().any(|v| v == "Closed"));
}

// ─── Union enum → anyOf schema ────────────────────────────────────

#[derive(BamlType)]
struct SchWeather {
    pub location: String,
}

#[derive(BamlType)]
struct SchCalc {
    pub expression: String,
}

#[derive(BamlType)]
#[baml(union)]
enum SchToolChoice {
    Weather(SchWeather),
    Calc(SchCalc),
}

#[test]
fn union_enum_schema() {
    let schema = SchToolChoice::json_schema_inline();
    let any_of = schema["anyOf"].as_array().expect("anyOf array");
    assert_eq!(any_of.len(), 2);
    // Each entry is an object schema
    assert!(any_of.iter().all(|s| s["type"] == "object"));
}

// ─── root_json_schema adds $schema pointer ────────────────────────

#[test]
fn root_schema_has_schema_pointer() {
    use baml_derive_core::root_json_schema;
    let schema = root_json_schema::<SchSimpleInput>();
    assert!(schema["$schema"].is_string(), "$schema should be set");
    assert!(
        schema["$schema"]
            .as_str()
            .unwrap()
            .contains("json-schema.org")
    );
}

// ─── () → empty schema (any) ─────────────────────────────────────

#[test]
fn unit_type_any_schema() {
    let schema = <()>::json_schema_inline();
    assert!(
        schema.as_object().map(|o| o.is_empty()).unwrap_or(false),
        "() should be {{}}"
    );
}
