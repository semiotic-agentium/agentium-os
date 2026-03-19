//! Schema and type helpers for tools.
//!
//! This module provides the [`ToolType`] supertrait and helper functions that
//! combine JSON Schema and TypeScript declaration generation. Both are now
//! provided by `#[derive(BamlType)]` via the `JsonSchemaType` and `TsType`
//! traits in `baml-derive-core`, replacing the external `schemars` and
//! `ts-rs` crates.

use baml_derive_core::{JsonSchemaType, TsType};
use serde_json::Value;

/// Supertrait that all tool input/output types must satisfy.
///
/// Requires `JsonSchemaType` (for tool call validation) and `TsType` (for SDK
/// TypeScript generation). Both are derived automatically by `#[derive(BamlType)]`.
pub trait ToolType: JsonSchemaType + TsType + Send + Sync + 'static {}

impl<T> ToolType for T where T: JsonSchemaType + TsType + Send + Sync + 'static {}

/// Generate the root JSON Schema document for `T`.
///
/// Wraps the inline schema from `T::json_schema_inline()` with a `$schema`
/// pointer and returns a `serde_json::Value` ready for tool validation.
pub fn json_schema_value<T: JsonSchemaType>() -> Value {
    baml_derive_core::root_json_schema::<T>()
}

/// Generate a TypeScript declaration for `T` via `TsType`.
///
/// Returns `None` for the unit type `()` and other non-declarable types.
pub fn ts_decl<T: TsType>() -> Option<String> {
    T::ts_decl()
}

/// Return the TypeScript type name for `T`.
pub fn ts_name<T: TsType>() -> String {
    T::ts_type_name().to_string()
}
