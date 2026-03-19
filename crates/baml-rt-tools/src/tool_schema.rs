//! Schema and type helpers for tools.

use schemars::JsonSchema;
use serde_json::Value;
use ts_rs::TS;

pub trait ToolType: JsonSchema + TS + Send + Sync + 'static {}

impl<T> ToolType for T where T: JsonSchema + TS + Send + Sync + 'static {}

/// Typed self-description for tool inputs. Each input variant knows how to
/// describe itself as natural language prose for drift scoring and context
/// summarisation. Implement on OpenInput types for session-level description
/// and on Send/Read Input types for action-level description.
pub trait DescribeAction {
    fn describe(&self) -> String;
}

impl DescribeAction for () {
    fn describe(&self) -> String {
        String::new()
    }
}

pub fn json_schema_value<T: JsonSchema>() -> Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(&schema).unwrap_or(Value::Null)
}

pub fn ts_decl<T: TS>() -> Option<String> {
    // Unit type () cannot be declared in TypeScript - return None
    if std::any::type_name::<T>() == "()" {
        return None;
    }
    Some(T::decl())
}

pub fn ts_name<T: TS>() -> String {
    // Unit type () - return empty string so BAML generator can skip the field
    if std::any::type_name::<T>() == "()" {
        return "()".to_string(); // Keep as () for BAML generator to detect and skip
    }
    T::name().to_string()
}
