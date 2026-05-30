// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Rust → JSON Schema resolution at the AST level.
//!
//! Mirrors `resolve.rs` and `ts_resolve.rs` but emits `serde_json::Value`
//! expressions (as `TokenStream`) for JSON Schema generation, inserted into
//! `JsonSchemaType` impls.
//!
//! Every function here returns a `TokenStream` that evaluates to a
//! `serde_json::Value` at the call site (i.e. inside the generated `impl`).
//! No `serde_json` runtime dependency is required in this proc-macro crate.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

/// Returns `true` when `ty` is `Option<_>` at the top level.
///
/// Used to determine which struct fields should appear in JSON Schema `required`.
pub(crate) fn is_option_type(ty: &Type) -> bool {
    if let Type::Path(tp) = ty
        && let Some(last) = tp.path.segments.last()
    {
        return last.ident == "Option";
    }
    false
}

/// Generate a `TokenStream` expression that evaluates to a `serde_json::Value`
/// representing the inline JSON Schema for the given Rust type.
pub(crate) fn resolve_schema_tokens(ty: &Type) -> Result<TokenStream, syn::Error> {
    match ty {
        Type::Path(type_path) => resolve_schema_path_tokens(type_path),
        Type::Reference(type_ref) => resolve_schema_tokens(&type_ref.elem),
        _ => Err(syn::Error::new_spanned(
            ty,
            "unsupported type for JSON Schema derivation",
        )),
    }
}

fn resolve_schema_path_tokens(type_path: &syn::TypePath) -> Result<TokenStream, syn::Error> {
    let path = &type_path.path;
    let last_segment = path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(path, "empty path cannot be resolved to a JSON Schema type")
    })?;

    let ident = &last_segment.ident;
    let ident_str = ident.to_string();

    // Primitives — emit a literal serde_json::json!({...}) expression.
    if let Some(schema_expr) = schema_primitive_tokens(&ident_str) {
        return Ok(schema_expr);
    }

    // Special: serde_json::Value (or any bare `Value` ident) → "any" schema
    if ident_str == "Value" {
        return Ok(quote! { ::serde_json::json!({}) });
    }

    // Generic wrappers
    match ident_str.as_str() {
        "Option" => {
            let inner = extract_single_generic_arg(last_segment)?;
            let inner_tokens = resolve_schema_tokens(&inner)?;
            return Ok(quote! {
                {
                    let __inner = #inner_tokens;
                    ::serde_json::json!({"anyOf": [__inner, {"type": "null"}]})
                }
            });
        }
        "Vec" => {
            let inner = extract_single_generic_arg(last_segment)?;
            let inner_tokens = resolve_schema_tokens(&inner)?;
            return Ok(quote! {
                {
                    let __items = #inner_tokens;
                    ::serde_json::json!({"type": "array", "items": __items})
                }
            });
        }
        "HashMap" | "BTreeMap" => {
            let (_key, val) = extract_two_generic_args(last_segment)?;
            let val_tokens = resolve_schema_tokens(&val)?;
            return Ok(quote! {
                {
                    let __add_props = #val_tokens;
                    ::serde_json::json!({"type": "object", "additionalProperties": __add_props})
                }
            });
        }
        "Box" => {
            let inner = extract_single_generic_arg(last_segment)?;
            return resolve_schema_tokens(&inner);
        }
        _ => {}
    }

    // Unknown generics → fall back to "any" rather than a hard error.
    if has_generic_args(last_segment) {
        return Ok(quote! { ::serde_json::json!({}) });
    }

    // User-defined type — delegate to its `JsonSchemaType` impl.
    Ok(quote! {
        <#ident as ::baml_derive_core::JsonSchemaType>::json_schema_inline()
    })
}

/// Return a `TokenStream` for the JSON Schema of a Rust primitive type,
/// or `None` if the type is not a known primitive.
///
/// Each returned expression evaluates to a `serde_json::Value` at runtime.
fn schema_primitive_tokens(ident: &str) -> Option<TokenStream> {
    match ident {
        "String" | "str" => Some(quote! { ::serde_json::json!({"type": "string"}) }),
        "bool" => Some(quote! { ::serde_json::json!({"type": "boolean"}) }),
        "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64" | "u128" | "isize"
        | "usize" => Some(quote! { ::serde_json::json!({"type": "integer"}) }),
        "f32" | "f64" => Some(quote! { ::serde_json::json!({"type": "number"}) }),
        _ => None,
    }
}

fn extract_single_generic_arg(segment: &syn::PathSegment) -> Result<Type, syn::Error> {
    match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => {
            let first = args.args.first().ok_or_else(|| {
                syn::Error::new_spanned(segment, "expected at least one generic argument")
            })?;
            match first {
                syn::GenericArgument::Type(ty) => Ok(ty.clone()),
                _ => Err(syn::Error::new_spanned(
                    first,
                    "expected a type argument, not a lifetime or const",
                )),
            }
        }
        _ => Err(syn::Error::new_spanned(
            segment,
            "expected angle-bracketed generic arguments",
        )),
    }
}

fn extract_two_generic_args(segment: &syn::PathSegment) -> Result<(Type, Type), syn::Error> {
    match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => {
            if args.args.len() < 2 {
                return Err(syn::Error::new_spanned(
                    segment,
                    "expected two generic arguments for map type",
                ));
            }
            let key = match &args.args[0] {
                syn::GenericArgument::Type(ty) => ty.clone(),
                other => return Err(syn::Error::new_spanned(other, "expected a type argument")),
            };
            let val = match &args.args[1] {
                syn::GenericArgument::Type(ty) => ty.clone(),
                other => return Err(syn::Error::new_spanned(other, "expected a type argument")),
            };
            Ok((key, val))
        }
        _ => Err(syn::Error::new_spanned(
            segment,
            "expected angle-bracketed generic arguments",
        )),
    }
}

fn has_generic_args(segment: &syn::PathSegment) -> bool {
    !matches!(segment.arguments, syn::PathArguments::None)
}

fn peel_reference(ty: &Type) -> &Type {
    match ty {
        Type::Reference(r) => peel_reference(&r.elem),
        t => t,
    }
}

/// `true` when the field is `Option<Vec<_>>` (with or without leading `&`).
pub(crate) fn vec_or_one_is_optional(ty: &Type) -> bool {
    let ty = peel_reference(ty);
    if let Type::Path(tp) = ty
        && let Some(last) = tp.path.segments.last()
    {
        return last.ident == "Option";
    }
    false
}

/// Element type `T` for `#[baml(vec_or_one)]` on `Vec<T>` or `Option<Vec<T>>`.
pub(crate) fn vec_or_one_element_type(ty: &Type) -> Result<Type, syn::Error> {
    let ty = peel_reference(ty);
    let Type::Path(tp) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "`#[baml(vec_or_one)]` requires field type `Vec<T>` or `Option<Vec<T>>`",
        ));
    };
    let last = tp
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(ty, "empty path"))?;

    if last.ident == "Option" {
        let inner = extract_single_generic_arg(last)?;
        let inner = peel_reference(&inner);
        let Type::Path(inner_path) = inner else {
            return Err(syn::Error::new_spanned(
                inner,
                "`#[baml(vec_or_one)]` expects Option<Vec<T>>",
            ));
        };
        let inner_last = inner_path.path.segments.last().ok_or_else(|| {
            syn::Error::new_spanned(inner, "`#[baml(vec_or_one)]` expects Option<Vec<T>>")
        })?;
        if inner_last.ident != "Vec" {
            return Err(syn::Error::new_spanned(
                inner,
                "`#[baml(vec_or_one)]` expects Option<Vec<T>>",
            ));
        }
        return extract_single_generic_arg(inner_last);
    }

    if last.ident == "Vec" {
        return extract_single_generic_arg(last);
    }

    Err(syn::Error::new_spanned(
        ty,
        "`#[baml(vec_or_one)]` requires field type `Vec<T>` or `Option<Vec<T>>`",
    ))
}

/// JSON Schema for `Vec<T>` / `Option<Vec<T>>` when wire may send one `T` or an array (BAML `T | T[]`).
pub(crate) fn resolve_schema_tokens_for_vec_or_one_field(
    field_ty: &Type,
) -> Result<TokenStream, syn::Error> {
    let elem_ty = vec_or_one_element_type(field_ty)?;
    let item_tokens = resolve_schema_tokens(&elem_ty)?;
    let optional = vec_or_one_is_optional(field_ty);

    let one_of_block = quote! {
        {
            let __item = #item_tokens;
            ::serde_json::json!({"oneOf": [__item.clone(), {"type": "array", "items": __item}]})
        }
    };

    if optional {
        Ok(quote! {
            {
                let __inner = #one_of_block;
                ::serde_json::json!({"anyOf": [__inner, {"type": "null"}]})
            }
        })
    } else {
        Ok(one_of_block)
    }
}
