//! Core types and rendering for the `BamlType` derive macro system.
//!
//! This crate provides:
//! - The [`BamlType`] trait that derive-annotated types implement
//! - Structured definition types ([`BamlDefinition`], [`BamlClassDef`], etc.)
//! - BAML text rendering via [`render_baml_types`]
//! - [`BamlFileOutput`] and [`BamlRenderTarget`] for `generate_baml!()` integration

pub mod render;
pub mod types;

pub use render::render_baml_types;
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
