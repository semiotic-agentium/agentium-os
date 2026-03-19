//! Schema and type helpers for tools.

use baml_derive_core::TsType;
use schemars::JsonSchema;
use serde_json::Value;
use ts_rs::TS;

/// Supertrait that all tool input/output types must satisfy.
///
/// A type must provide JSON Schema (for tool validation), TypeScript declaration
/// (for SDK generation), and BAML-derived TypeScript output — all in one.
pub trait ToolType: JsonSchema + TS + TsType + Send + Sync + 'static {}

impl<T> ToolType for T where T: JsonSchema + TS + TsType + Send + Sync + 'static {}

pub fn json_schema_value<T: JsonSchema>() -> Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(&schema).unwrap_or(Value::Null)
}

/// Generate a TypeScript declaration for `T` via the `ts-rs` `TS` trait.
///
/// Returns `None` for unit type `()` which has no TypeScript representation.
pub fn ts_decl<T: TS>() -> Option<String> {
    // Unit type () cannot be declared in TypeScript - return None
    if std::any::type_name::<T>() == "()" {
        return None;
    }
    Some(T::decl())
}

/// Generate a TypeScript declaration for `T` via the `TsType` trait derived
/// by `#[derive(BamlType)]`.
///
/// Prefer this over [`ts_decl`] for types annotated with `#[derive(BamlType)]`,
/// as it respects `#[baml(skip)]` and produces output that aligns with the
/// BAML-generated TypeScript declarations.
///
/// Returns `None` for the unit type `()`.
pub fn ts_decl_from_trait<T: TsType>() -> Option<String> {
    T::ts_decl()
}

/// Return the TypeScript type name for `T` via `TsType`.
pub fn ts_name_from_trait<T: TsType>() -> &'static str {
    T::ts_type_name()
}

pub fn ts_name<T: TS>() -> String {
    // Unit type () - return empty string so BAML generator can skip the field
    if std::any::type_name::<T>() == "()" {
        return "()".to_string(); // Keep as () for BAML generator to detect and skip
    }
    T::name().to_string()
}
