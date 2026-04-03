//! Rust → BAML type resolution at the AST level.
//!
//! Since proc macros only see token trees (not resolved types), we pattern-match
//! on the final path segment to map Rust types to BAML types. For user-defined
//! types (those that also derive `BamlType`), we fall back to the type name
//! itself — BAML will resolve it from the same `baml_src/` directory.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use crate::schema_resolve::{vec_or_one_element_type, vec_or_one_is_optional};

/// Generate a `TokenStream` expression that evaluates to the BAML type string
/// for the given Rust type. This is inserted into the generated `BamlType` impl.
///
/// Returns `Err` for types that cannot be mapped (unknown generic wrappers).
pub(crate) fn resolve_type_tokens(ty: &Type) -> Result<TokenStream, syn::Error> {
    match ty {
        Type::Path(type_path) => resolve_path_tokens(type_path),
        Type::Reference(type_ref) => {
            // &str, &T → resolve inner type
            resolve_type_tokens(&type_ref.elem)
        }
        Type::Tuple(tuple) if tuple.elems.is_empty() => {
            // () → unit, no BAML representation; caller should handle skip
            Err(syn::Error::new_spanned(
                ty,
                "unit type `()` has no BAML representation; consider `#[baml(skip)]`",
            ))
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "unsupported type for BAML derivation",
        )),
    }
}

fn resolve_path_tokens(type_path: &syn::TypePath) -> Result<TokenStream, syn::Error> {
    let path = &type_path.path;

    // Get the last segment (handles `std::string::String`, `Option<T>`, etc.)
    let last_segment = path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(path, "empty path cannot be resolved to a BAML type")
    })?;

    let ident = &last_segment.ident;
    let ident_str = ident.to_string();

    // Check for primitive types first.
    if let Some(baml_primitive) = primitive_mapping(&ident_str) {
        let lit = baml_primitive;
        return Ok(quote! { ::std::string::String::from(#lit) });
    }

    // `serde_json::Value` must be made explicit at the BAML boundary.
    //
    // Tool-facing opaque JSON should use `baml_rt_tools::OpaqueJson` so the
    // generated schema/codegen layer can preserve the intent instead of silently
    // degrading to a plain string.
    if ident_str == "Value" {
        return Err(syn::Error::new_spanned(
            ident,
            "bare serde_json::Value cannot be derived into BAML; use baml_rt_tools::OpaqueJson or an explicit #[baml(type = \"...\")] override",
        ));
    }

    // Check for known generic wrappers.
    match ident_str.as_str() {
        "Option" => {
            let inner = extract_single_generic_arg(last_segment)?;
            let inner_tokens = resolve_type_tokens(&inner)?;
            return Ok(quote! { format!("{}?", #inner_tokens) });
        }
        "Vec" => {
            let inner = extract_single_generic_arg(last_segment)?;
            let inner_tokens = resolve_type_tokens(&inner)?;
            return Ok(quote! { format!("{}[]", #inner_tokens) });
        }
        "HashMap" | "BTreeMap" => {
            let (key, val) = extract_two_generic_args(last_segment)?;
            let key_tokens = resolve_type_tokens(&key)?;
            let val_tokens = resolve_type_tokens(&val)?;
            return Ok(quote! { format!("map<{}, {}>", #key_tokens, #val_tokens) });
        }
        "Box" => {
            // Box<T> → resolve inner T (transparent wrapper)
            let inner = extract_single_generic_arg(last_segment)?;
            return resolve_type_tokens(&inner);
        }
        _ => {}
    }

    // Check for unknown generics (strict mode: reject).
    if has_generic_args(last_segment) {
        return Err(syn::Error::new_spanned(
            last_segment,
            format!(
                "unknown generic wrapper `{}` cannot be automatically resolved to a BAML type; \
                 use `#[baml(type = \"...\")]` to provide an explicit override",
                ident_str
            ),
        ));
    }

    // User-defined type — emit the type name as-is.
    // It must itself derive `BamlType` for BAML to resolve it.
    Ok(quote! { ::std::string::String::from(#ident_str) })
}

/// Map a Rust primitive/standard type name to its BAML equivalent.
fn primitive_mapping(ident: &str) -> Option<&'static str> {
    match ident {
        "String" | "str" => Some("string"),
        "bool" => Some("bool"),
        "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64" | "u128" | "isize"
        | "usize" => Some("int"),
        "f32" | "f64" => Some("float"),
        _ => None,
    }
}

/// Extract the single generic argument from `Foo<T>`.
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

/// Extract two generic arguments from `Foo<K, V>`.
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
                other => {
                    return Err(syn::Error::new_spanned(other, "expected a type argument"));
                }
            };
            let val = match &args.args[1] {
                syn::GenericArgument::Type(ty) => ty.clone(),
                other => {
                    return Err(syn::Error::new_spanned(other, "expected a type argument"));
                }
            };
            Ok((key, val))
        }
        _ => Err(syn::Error::new_spanned(
            segment,
            "expected angle-bracketed generic arguments",
        )),
    }
}

/// Check whether a path segment has generic arguments.
fn has_generic_args(segment: &syn::PathSegment) -> bool {
    !matches!(segment.arguments, syn::PathArguments::None)
}

/// BAML `T | T[]` or `(T | T[])?` for `#[baml(vec_or_one)]` fields.
pub(crate) fn resolve_type_tokens_for_vec_or_one_field(
    field_ty: &Type,
) -> Result<TokenStream, syn::Error> {
    let elem_ty = vec_or_one_element_type(field_ty)?;
    let elem_tokens = resolve_type_tokens(&elem_ty)?;
    let optional = vec_or_one_is_optional(field_ty);
    if optional {
        Ok(quote! {
            {
                let __e = #elem_tokens;
                format!("({} | {}[])?", __e, __e)
            }
        })
    } else {
        Ok(quote! {
            {
                let __e = #elem_tokens;
                format!("({} | {}[])", __e, __e)
            }
        })
    }
}
