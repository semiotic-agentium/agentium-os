// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Rust → TypeScript type resolution at the AST level.
//!
//! Mirrors `resolve.rs` for BAML but emits TypeScript type expressions instead.
//! Since proc macros operate only on token trees (not resolved types), we
//! pattern-match on the final path segment to map Rust types to TypeScript.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use crate::schema_resolve::{vec_or_one_element_type, vec_or_one_is_optional};

/// Generate a `TokenStream` expression that evaluates to the TypeScript type
/// string for the given Rust type.  Inserted into the generated `TsType` impl.
///
/// Returns `Err` for types that cannot be automatically mapped.
pub(crate) fn resolve_ts_type_tokens(ty: &Type) -> Result<TokenStream, syn::Error> {
    match ty {
        Type::Path(type_path) => resolve_ts_path_tokens(type_path),
        Type::Reference(type_ref) => {
            // &str, &T → resolve inner type
            resolve_ts_type_tokens(&type_ref.elem)
        }
        Type::Tuple(tuple) if tuple.elems.is_empty() => {
            // () → unit; caller must skip
            Err(syn::Error::new_spanned(
                ty,
                "unit type `()` has no TypeScript representation; consider `#[baml(skip)]`",
            ))
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "unsupported type for TypeScript derivation",
        )),
    }
}

fn resolve_ts_path_tokens(type_path: &syn::TypePath) -> Result<TokenStream, syn::Error> {
    let path = &type_path.path;

    let last_segment = path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(path, "empty path cannot be resolved to a TypeScript type")
    })?;

    let ident = &last_segment.ident;
    let ident_str = ident.to_string();

    // Primitives
    if let Some(ts_prim) = ts_primitive_mapping(&ident_str) {
        let lit = ts_prim;
        return Ok(quote! { ::std::string::String::from(#lit) });
    }

    // Special: serde_json::Value (bare `Value` ident) → TypeScript `any`
    if ident_str == "Value" {
        return Ok(quote! { ::std::string::String::from("any") });
    }

    // Generic wrappers
    match ident_str.as_str() {
        "Option" => {
            let inner = extract_single_generic_arg(last_segment)?;
            let inner_tokens = resolve_ts_type_tokens(&inner)?;
            return Ok(quote! { format!("{} | null", #inner_tokens) });
        }
        "Vec" => {
            let inner = extract_single_generic_arg(last_segment)?;
            let inner_tokens = resolve_ts_type_tokens(&inner)?;
            return Ok(quote! { format!("{}[]", #inner_tokens) });
        }
        "HashMap" | "BTreeMap" => {
            let (_key, val) = extract_two_generic_args(last_segment)?;
            // TypeScript Record keys are always `string` in practice; we use string
            // regardless of the map key type, matching the JSON serialisation default.
            let val_tokens = resolve_ts_type_tokens(&val)?;
            return Ok(quote! { format!("Record<string, {}>", #val_tokens) });
        }
        "Box" => {
            // Box<T> is transparent in TypeScript
            let inner = extract_single_generic_arg(last_segment)?;
            return resolve_ts_type_tokens(&inner);
        }
        _ => {}
    }

    // Unknown generic wrapper → reject with helpful message
    if has_generic_args(last_segment) {
        return Err(syn::Error::new_spanned(
            last_segment,
            format!(
                "unknown generic wrapper `{ident_str}` cannot be automatically resolved to a \
                 TypeScript type; use `#[baml(type = \"...\")]` to skip or override"
            ),
        ));
    }

    // User-defined type — emit the type name as-is
    Ok(quote! { ::std::string::String::from(#ident_str) })
}

/// Map a Rust primitive/standard type to its TypeScript equivalent.
fn ts_primitive_mapping(ident: &str) -> Option<&'static str> {
    match ident {
        "String" | "str" => Some("string"),
        "bool" => Some("boolean"),
        "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64" | "u128" | "isize"
        | "usize" => Some("number"),
        "f32" | "f64" => Some("number"),
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

/// TypeScript for `Vec<T>` / `Option<Vec<T>>` when wire may send one `T` or `T[]`.
pub(crate) fn resolve_ts_tokens_for_vec_or_one_field(
    field_ty: &Type,
) -> Result<TokenStream, syn::Error> {
    let elem_ty = vec_or_one_element_type(field_ty)?;
    let elem_ts = resolve_ts_type_tokens(&elem_ty)?;
    let optional = vec_or_one_is_optional(field_ty);
    if optional {
        Ok(quote! {
            {
                let __e = #elem_ts;
                format!("({} | {}[]) | null", __e, __e)
            }
        })
    } else {
        Ok(quote! {
            {
                let __e = #elem_ts;
                format!("{} | {}[]", __e, __e)
            }
        })
    }
}
