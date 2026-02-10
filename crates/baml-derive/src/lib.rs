//! Procedural derive macro for generating BAML type definitions from Rust types.
//!
//! # Usage
//!
//! ```rust,ignore
//! use baml_derive::BamlType;
//!
//! #[derive(BamlType)]
//! struct MyInput {
//!     pub name: String,
//!     pub count: Option<i32>,
//! }
//! ```
//!
//! See the `baml-derive-core` crate for the trait definition and rendering.

mod attrs;
mod expand;
mod resolve;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Derive macro that generates a `BamlType` implementation for the annotated type.
///
/// # Supported Types
///
/// - **Structs** with named fields → BAML `class`
/// - **Enums** with unit variants → BAML `enum`
/// - **Enums** with `#[baml(union)]` and newtype variants → BAML `type Foo = A | B`
///
/// # Container Attributes
///
/// - `#[baml(dynamic)]` — adds `@@dynamic` to the BAML class
/// - `#[baml(union)]` — generates a BAML union type alias instead of an enum
///
/// # Field Attributes
///
/// - `#[baml(alias = "...")]` — adds `@alias("...")`
/// - `#[baml(description = "...")]` — adds `@description("...")`
/// - `#[baml(skip)]` — omits the field from the BAML definition
/// - `#[baml(type = "...")]` — overrides automatic type resolution
///
/// # Variant Attributes (for enums)
///
/// - `#[baml(alias = "...")]` — adds `@alias("...")`
/// - `#[baml(description = "...")]` — adds `@description("...")`
/// - `#[baml(skip)]` — omits the variant from the BAML definition
#[proc_macro_derive(BamlType, attributes(baml))]
pub fn derive_baml_type(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match expand::expand_derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
