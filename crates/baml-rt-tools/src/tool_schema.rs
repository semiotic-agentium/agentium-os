// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Schema and type helpers for tools.
//!
//! This module provides the [`ToolType`] supertrait and helper functions that
//! combine JSON Schema and TypeScript declaration generation. Both are now
//! provided by `#[derive(BamlType)]` via the `JsonSchemaType` and `TsType`
//! traits in `baml-derive-core`, replacing the external `schemars` and
//! `ts-rs` crates.

use baml_derive_core::{JsonSchemaType, TsType};
use serde_json::Value;

/// Compact, structured identity for a typed tool action.
///
/// This is intentionally separate from [`DescribeAction`]: `DescribeAction` is natural-language
/// prose for drift scoring and context summarisation, while action identity is a short,
/// function-signature-like description suitable for archive headers and model reuse decisions.
///
/// Implementors should include only the minimal input-derived fields needed to identify whether an
/// existing result is relevant. Do **not** include every argument. Prefer stable IDs, concise query
/// strings, and short names/titles when they are the best discriminator. Avoid long descriptions,
/// large arrays/objects, auth material, secrets, or output/result-derived information.
///
/// Field order is significant: renderers may keep only the first few fields to preserve prompt
/// budget. Put the strongest identifiers first.
#[derive(Debug, Clone)]
pub struct ActionIdentity {
    /// Stable snake_case action/variant name, e.g. `list_tasks` or `get_page_blocks`.
    pub name: Option<&'static str>,
    /// Minimal input-derived identity fields in priority order.
    pub fields: Vec<(&'static str, Value)>,
}

impl ActionIdentity {
    pub fn new(name: impl Into<Option<&'static str>>, fields: Vec<(&'static str, Value)>) -> Self {
        Self {
            name: name.into(),
            fields,
        }
    }
}

/// Optional helper trait for enum-style tool inputs.
///
/// This trait is not a `BamlTool::Input` bound. Tool implementations opt in by delegating their
/// tool-level `BamlTool::action_identity` hook to this typed input helper.
pub trait DescribeActionIdentity {
    fn action_identity(&self) -> ActionIdentity;
}

/// Implement [`DescribeActionIdentity`] for enum-style tool inputs.
///
/// The macro is intentionally explicit: tool authors choose the stable action name and the small
/// ordered set of fields that identify reuse. It does not infer fields from type structure.
///
/// This helper supports the internal BAML union shape used by current official tools: enum variants
/// with exactly one tuple payload, e.g. `ListTasks(ListTasksInput)`. It does not support unit
/// variants, struct variants, or tuple variants with multiple payloads; implement the trait manually
/// for those shapes.
///
/// ```ignore
/// baml_rt_tools::impl_describe_action_identity! {
///     for ClickUpInput {
///         ListTeams(_) => "list_teams" {},
///         ListTasks(p) => "list_tasks" { list_id: p.list_id },
///         CreateTask(p) => "create_task" { list_id: p.list_id, name: p.name },
///     }
/// }
/// ```
#[macro_export]
macro_rules! impl_describe_action_identity {
    (
        for $ty:ty {
            $(
                $variant:ident ( $binding:pat ) => $name:literal { $( $field:ident : $value:expr ),* $(,)? }
            ),* $(,)?
        }
    ) => {
        impl $crate::DescribeActionIdentity for $ty {
            fn action_identity(&self) -> $crate::ActionIdentity {
                match self {
                    $(
                        Self::$variant($binding) => $crate::ActionIdentity::new(
                            Some($name),
                            vec![
                                $(
                                    (
                                        stringify!($field),
                                        ::serde_json::to_value(&$value).unwrap_or(::serde_json::Value::Null),
                                    ),
                                )*
                            ],
                        ),
                    )*
                }
            }
        }
    };
}

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
