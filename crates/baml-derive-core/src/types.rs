// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! BAML definition types used by the `BamlType` derive macro.
//!
//! These structs represent the structured intermediate form of BAML
//! class/enum definitions. The derive macro generates code that constructs
//! these, and `render()` converts them to BAML text.

use crate::render;

/// A complete BAML type definition — either a class or an enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BamlDefinition {
    /// A BAML `class` (from a Rust struct).
    Class(BamlClassDef),
    /// A BAML `enum` (from a Rust enum with unit variants).
    Enum(BamlEnumDef),
    /// A BAML union type alias (from a Rust enum with newtype variants
    /// annotated `#[baml(union)]`).
    Union(BamlUnionDef),
}

impl BamlDefinition {
    /// Render this definition to a BAML text string.
    pub fn render(&self) -> String {
        render::render_definition(self)
    }

    /// The BAML type name for this definition.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Class(c) => c.name,
            Self::Enum(e) => e.name,
            Self::Union(u) => u.name,
        }
    }
}

/// A BAML `class` definition derived from a Rust struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BamlClassDef {
    /// The BAML class name (typically the Rust struct name).
    pub name: &'static str,
    /// Doc comment extracted from `///` on the struct.
    pub doc: Option<&'static str>,
    /// The fields of the class.
    pub fields: Vec<BamlFieldDef>,
    /// Whether `@@dynamic` is set on this class.
    pub dynamic: bool,
}

/// A single field within a BAML class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BamlFieldDef {
    /// The BAML field name (typically the Rust field name).
    pub name: &'static str,
    /// The resolved BAML type string, e.g. `"string?"`, `"int[]"`, `"ClickUpAction"`.
    pub baml_type: String,
    /// Optional `@alias("...")` override.
    pub alias: Option<&'static str>,
    /// Optional `@description("...")` annotation.
    pub description: Option<&'static str>,
    /// Whether this field is `@skip`-ped in BAML output.
    pub skip: bool,
}

/// A BAML `enum` definition derived from a Rust enum with unit variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BamlEnumDef {
    /// The BAML enum name (typically the Rust enum name).
    pub name: &'static str,
    /// Doc comment extracted from `///` on the enum.
    pub doc: Option<&'static str>,
    /// The variants of the enum.
    pub variants: Vec<BamlVariantDef>,
}

/// A single variant within a BAML enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BamlVariantDef {
    /// The BAML variant name (typically the Rust variant name).
    pub name: &'static str,
    /// Optional `@alias("...")` override.
    pub alias: Option<&'static str>,
    /// Optional `@description("...")` annotation.
    pub description: Option<&'static str>,
    /// Whether this variant is `@skip`-ped in BAML output.
    pub skip: bool,
}

/// A BAML union type alias derived from a Rust enum with `#[baml(union)]`.
///
/// Generates: `type Foo = A | B | C`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BamlUnionDef {
    /// The BAML type alias name.
    pub name: &'static str,
    /// Doc comment extracted from `///` on the enum.
    pub doc: Option<&'static str>,
    /// The constituent BAML type names.
    pub variants: Vec<&'static str>,
}

/// Output from `generate_baml!()` — a file path and its rendered content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BamlFileOutput {
    /// Relative path to the `.baml` file (relative to `CARGO_MANIFEST_DIR`).
    pub relative_path: String,
    /// The complete rendered BAML file content.
    pub content: String,
}

/// Registration entry for `inventory`-based discovery of `generate_baml!()` targets.
pub struct BamlRenderTarget {
    /// Function that produces the file output.
    pub render_fn: fn() -> BamlFileOutput,
    /// The `CARGO_MANIFEST_DIR` of the crate that registered this target.
    pub manifest_dir: &'static str,
}

inventory::collect!(BamlRenderTarget);
