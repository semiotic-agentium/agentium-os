//! Parsing of `#[baml(...)]` attributes on structs, enums, fields, and variants.

use syn::{Attribute, Expr, ExprLit, Lit, Meta, MetaNameValue};

/// Container-level (struct/enum) attributes from `#[baml(...)]`.
#[derive(Debug, Default)]
pub(crate) struct ContainerAttrs {
    /// `#[baml(dynamic)]` → BAML `@@dynamic`
    pub dynamic: bool,
    /// `#[baml(union)]` → generate `type Foo = A | B` instead of `enum Foo`
    pub union: bool,
}

/// Field-level attributes from `#[baml(...)]`.
#[derive(Debug, Default)]
pub(crate) struct FieldAttrs {
    /// `#[baml(alias = "...")]`
    pub alias: Option<String>,
    /// `#[baml(description = "...")]`
    pub description: Option<String>,
    /// `#[baml(skip)]`
    pub skip: bool,
    /// `#[baml(type = "...")]` — escape hatch for explicit BAML type override.
    pub type_override: Option<String>,
}

/// Variant-level attributes from `#[baml(...)]`.
#[derive(Debug, Default)]
pub(crate) struct VariantAttrs {
    /// `#[baml(alias = "...")]`
    pub alias: Option<String>,
    /// `#[baml(description = "...")]`
    pub description: Option<String>,
    /// `#[baml(skip)]`
    pub skip: bool,
}

/// Parse container-level `#[baml(...)]` attributes.
pub(crate) fn parse_container_attrs(attrs: &[Attribute]) -> syn::Result<ContainerAttrs> {
    let mut result = ContainerAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("baml") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("dynamic") {
                result.dynamic = true;
                return Ok(());
            }
            if meta.path.is_ident("union") {
                result.union = true;
                return Ok(());
            }
            Err(meta.error("unknown baml container attribute; expected `dynamic` or `union`"))
        })?;
    }

    Ok(result)
}

/// Parse field-level `#[baml(...)]` attributes.
pub(crate) fn parse_field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttrs> {
    let mut result = FieldAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("baml") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                result.skip = true;
                return Ok(());
            }
            if meta.path.is_ident("alias") {
                result.alias = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("description") {
                result.description = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("type") {
                result.type_override = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            Err(meta.error(
                "unknown baml field attribute; expected `alias`, `description`, `skip`, or `type`",
            ))
        })?;
    }

    Ok(result)
}

/// Parse variant-level `#[baml(...)]` attributes.
pub(crate) fn parse_variant_attrs(attrs: &[Attribute]) -> syn::Result<VariantAttrs> {
    let mut result = VariantAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("baml") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                result.skip = true;
                return Ok(());
            }
            if meta.path.is_ident("alias") {
                result.alias = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("description") {
                result.description = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            Err(meta.error(
                "unknown baml variant attribute; expected `alias`, `description`, or `skip`",
            ))
        })?;
    }

    Ok(result)
}

/// Extract the doc string from `/// ...` doc-comment attributes.
pub(crate) fn extract_doc_comment(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();

    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(MetaNameValue {
            value:
                Expr::Lit(ExprLit {
                    lit: Lit::Str(lit), ..
                }),
            ..
        }) = &attr.meta
        {
            lines.push(lit.value());
        }
    }

    if lines.is_empty() {
        return None;
    }

    // Doc attributes typically have a leading space: `/// foo` → `" foo"`.
    // We trim each line.
    let doc = lines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");

    Some(doc)
}

/// Helper: parse `= "value"` from a name-value meta item.
fn parse_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let value = meta.value()?;
    let lit: syn::LitStr = value.parse()?;
    Ok(lit.value())
}
