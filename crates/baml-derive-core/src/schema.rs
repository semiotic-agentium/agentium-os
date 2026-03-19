//! JSON Schema generation trait for types derived with `#[derive(BamlType)]`.

use serde_json::Value;

/// Trait implemented by types that can emit an inline JSON Schema value.
///
/// This is automatically derived via `#[derive(BamlType)]` from the
/// `baml-derive` crate, alongside the `BamlType` and `TsType` implementations.
/// It drives the `open_input_schema` / `input_schema` / `output_schema` fields
/// in `ToolFunctionMetadata`, replacing the `schemars` dependency.
///
/// # JSON Schema output shapes
///
/// | Rust form | JSON Schema |
/// |---|---|
/// | Struct with named fields | `{"type":"object","properties":{…},"required":[…]}` |
/// | Unit enum | `{"type":"string","enum":["A","B",…]}` |
/// | Newtype union enum (`#[baml(union)]`) | `{"anyOf":[{…},{…}]}` |
///
/// # Attribute interaction
///
/// - `#[baml(skip)]` suppresses a field from the schema `properties` entirely.
/// - `Option<T>` fields are excluded from `required`; all others are required.
/// - User-defined types appearing as field types must themselves implement
///   `JsonSchemaType` (guaranteed when they also derive `BamlType`).
pub trait JsonSchemaType: Send + Sync + 'static {
    /// Return an inline JSON Schema value for this type.
    ///
    /// For structs, this is a full `"type":"object"` schema with `properties`
    /// and `required`.  For enums and unions, it's a string-enum or `anyOf`
    /// schema respectively.
    fn json_schema_inline() -> Value;
}

/// `JsonSchemaType` for the Rust unit type `()`.
///
/// `()` maps to the empty schema `{}`, which accepts any JSON value.
impl JsonSchemaType for () {
    fn json_schema_inline() -> Value {
        serde_json::json!({})
    }
}

/// `JsonSchemaType` for `serde_json::Value`.
///
/// Maps to the empty schema `{}`, which accepts any JSON value — matching
/// the TypeScript `any` type.
impl JsonSchemaType for Value {
    fn json_schema_inline() -> Value {
        serde_json::json!({})
    }
}

/// Build a complete root JSON Schema for type `T`.
///
/// Wraps the inline schema from `T::json_schema_inline()` and adds the
/// `"$schema"` pointer so consumers receive a well-formed schema document.
pub fn root_json_schema<T: JsonSchemaType>() -> Value {
    let mut schema = T::json_schema_inline();
    if let Some(obj) = schema.as_object_mut() {
        obj.insert(
            "$schema".to_string(),
            Value::String("http://json-schema.org/draft-07/schema#".to_string()),
        );
    }
    schema
}
