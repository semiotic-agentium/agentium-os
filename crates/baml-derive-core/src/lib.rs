// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Core types and rendering for the `BamlType` derive macro system.
//!
//! This crate provides:
//! - The [`BamlType`] trait that derive-annotated types implement
//! - The [`TsType`] trait for TypeScript declaration generation
//! - Structured definition types ([`BamlDefinition`], [`BamlClassDef`], etc.)
//! - BAML text rendering via [`render_baml_types`]
//! - [`BamlFileOutput`] and [`BamlRenderTarget`] for `generate_baml!()` integration

pub mod render;
pub mod schema;
pub mod ts_render;
pub mod types;

pub use render::render_baml_types;
pub use schema::{JsonSchemaType, root_json_schema};
pub use ts_render::render_ts_declarations;
pub use types::{
    BamlClassDef, BamlDefinition, BamlEnumDef, BamlFieldDef, BamlFileOutput, BamlRenderTarget,
    BamlUnionDef, BamlVariantDef,
};

/// Trait implemented by types that can be represented as BAML definitions.
///
/// This is automatically derived via `#[derive(BamlType)]` from the
/// `baml-derive` crate. It provides both a structured definition and
/// a pre-rendered BAML string for embedding in tool metadata.
pub trait BamlType: Send + Sync + 'static {
    /// The BAML type name (e.g. `"ClickUpInput"`).
    fn baml_type_name() -> &'static str;

    /// Structured definition — used by `generate_baml!()` for file rendering.
    fn baml_definition() -> BamlDefinition;

    /// Pre-rendered BAML string — used by `ToolFunctionMetadata.baml_decl`.
    ///
    /// The default implementation calls `baml_definition().render()`.
    fn baml_decl() -> String {
        Self::baml_definition().render()
    }

    /// Collect all transitive BAML type dependency names.
    ///
    /// Used to ensure referenced types are included when rendering files.
    fn baml_dependencies() -> Vec<&'static str> {
        vec![]
    }
}

/// Trait implemented by types that can emit TypeScript type declarations.
///
/// This is automatically derived via `#[derive(BamlType)]` from the
/// `baml-derive` crate, alongside the `BamlType` implementation. It drives
/// the TypeScript declaration output used in `ToolFunctionMetadata` and the
/// `baml-runtime.d.ts` generated file.
///
/// # TypeScript output shapes
///
/// - Rust **struct** → `export interface Foo { field: string; count: number | null; }`
/// - Rust **unit enum** → `export type Status = "Open" | "Closed";`
/// - Rust **newtype union enum** (`#[baml(union)]`) → `export type Foo = TypeA | TypeB;`
///
/// # Attribute interaction
///
/// - `#[baml(skip)]` suppresses the field/variant from both BAML and TypeScript output.
/// - `#[baml(alias)]` and `#[baml(description)]` are BAML-only; for **struct** fields TypeScript
///   keeps the Rust field name. For **unit enums**, TypeScript string literals and JSON Schema
///   enum values follow one wire rule: `#[baml(alias)]` if set, else `#[serde(rename)]` /
///   container `rename_all`, else the Rust variant identifier.
pub trait TsType: Send + Sync + 'static {
    /// The TypeScript type name (mirrors the Rust type name).
    fn ts_type_name() -> &'static str;

    /// Pre-rendered TypeScript declaration string.
    ///
    /// Returns `None` for the unit type `()` and other types that have no
    /// standalone TypeScript declaration.
    fn ts_decl() -> Option<String>;

    /// Collect all transitive TypeScript type dependency names.
    ///
    /// Used to ensure referenced types are declared before this type when
    /// rendering a TypeScript output file.
    fn ts_dependencies() -> Vec<&'static str> {
        vec![]
    }
}

/// `TsType` for the Rust unit type `()`.
///
/// `()` has no TypeScript representation; `ts_decl()` returns `None`.
impl TsType for () {
    fn ts_type_name() -> &'static str {
        "()"
    }
    fn ts_decl() -> Option<String> {
        None
    }
}

/// `TsType` for `serde_json::Value`.
///
/// Maps to TypeScript `any`, as `serde_json::Value` can hold any JSON value.
impl TsType for serde_json::Value {
    fn ts_type_name() -> &'static str {
        "any"
    }
    fn ts_decl() -> Option<String> {
        None
    }
}
