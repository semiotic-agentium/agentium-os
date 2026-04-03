//! Code generation for `#[derive(BamlType)]`.
//!
//! Handles expansion for:
//! - Named structs → `BamlDefinition::Class` + TypeScript `export interface`
//! - Enums with unit variants → `BamlDefinition::Enum` + TypeScript string union
//! - Enums with `#[baml(union)]` and newtype variants → `BamlDefinition::Union` + TypeScript union

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DataEnum, DataStruct, DeriveInput, Fields, Type};

use crate::{
    attrs::{extract_doc_comment, parse_container_attrs, parse_field_attrs, parse_variant_attrs},
    resolve::{resolve_type_tokens, resolve_type_tokens_for_vec_or_one_field},
    schema_resolve::{
        is_option_type, resolve_schema_tokens, resolve_schema_tokens_for_vec_or_one_field,
        vec_or_one_element_type,
    },
    ts_resolve::{resolve_ts_tokens_for_vec_or_one_field, resolve_ts_type_tokens},
};

/// Main entry point: expand `#[derive(BamlType)]` for any supported data type.
pub(crate) fn expand_derive(input: &DeriveInput) -> Result<TokenStream, syn::Error> {
    let container_attrs = parse_container_attrs(&input.attrs)?;

    match &input.data {
        syn::Data::Struct(data) => expand_struct(input, data, &container_attrs),
        syn::Data::Enum(data) => {
            if container_attrs.union {
                expand_union_enum(input, data)
            } else {
                expand_enum(input, data)
            }
        }
        syn::Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "BamlType cannot be derived for Rust unions; only structs and enums are supported",
        )),
    }
}

/// Expand for a named struct → `BamlDefinition::Class`.
fn expand_struct(
    input: &DeriveInput,
    data: &DataStruct,
    container_attrs: &crate::attrs::ContainerAttrs,
) -> Result<TokenStream, syn::Error> {
    let name = &input.ident;
    let name_str = name.to_string();

    let doc = extract_doc_comment(&input.attrs);
    let doc_tokens = option_str_tokens(&doc);

    let dynamic = container_attrs.dynamic;

    let fields = match &data.fields {
        Fields::Named(named) => &named.named,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "BamlType can only be derived for structs with named fields",
            ));
        }
    };

    let mut field_tokens = Vec::new();
    // Each entry is `(field_name_str, ts_type_expr)` for non-skipped fields.
    let mut ts_field_pairs: Vec<(String, TokenStream)> = Vec::new();
    // Each entry is `(field_name_str, schema_expr, is_required)` for non-skipped fields.
    let mut schema_field_triples: Vec<(String, TokenStream, bool)> = Vec::new();
    let mut dep_names = Vec::new();
    let mut ts_dep_names: Vec<String> = Vec::new();

    for field in fields {
        let field_attrs = parse_field_attrs(&field.attrs)?;
        let field_name = field
            .ident
            .as_ref()
            .ok_or_else(|| syn::Error::new_spanned(field, "unnamed field"))?;
        let field_name_str = field_name.to_string();

        let alias_tokens = option_str_tokens(&field_attrs.alias);
        let desc_tokens = option_str_tokens(&field_attrs.description);
        let skip = field_attrs.skip;

        if field_attrs.vec_or_one {
            if skip {
                return Err(syn::Error::new_spanned(
                    field,
                    "`#[baml(vec_or_one)]` cannot be used with `#[baml(skip)]`",
                ));
            }
            if field_attrs.type_override.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "`#[baml(vec_or_one)]` cannot be combined with `#[baml(type = ...)]`",
                ));
            }
            if field_attrs.ts_type.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "`#[baml(vec_or_one)]` cannot be combined with `#[baml(ts_type = ...)]`",
                ));
            }
            let _ = vec_or_one_element_type(&field.ty)?;
        }

        // Resolve the BAML type string expression.
        let baml_type_expr = if let Some(ref override_type) = field_attrs.type_override {
            quote! { ::std::string::String::from(#override_type) }
        } else if skip {
            quote! { ::std::string::String::new() }
        } else if field_attrs.vec_or_one {
            resolve_type_tokens_for_vec_or_one_field(&field.ty)?
        } else {
            resolve_type_tokens(&field.ty)?
        };

        // Collect user-type dependencies (non-primitive field types).
        if !skip
            && field_attrs.type_override.is_none()
            && let Some(dep) = extract_user_type_dep(&field.ty)
        {
            dep_names.push(dep.clone());
            ts_dep_names.push(dep);
        }

        if !skip {
            // TypeScript — respect explicit ts_type override, then auto-resolve.
            let ts_type_expr = if let Some(ref explicit) = field_attrs.ts_type {
                quote! { ::std::string::String::from(#explicit) }
            } else if field_attrs.type_override.is_some() {
                quote! { ::std::string::String::from("any") }
            } else if field_attrs.vec_or_one {
                match resolve_ts_tokens_for_vec_or_one_field(&field.ty) {
                    Ok(expr) => expr,
                    Err(_) => quote! { ::std::string::String::from("any") },
                }
            } else {
                match resolve_ts_type_tokens(&field.ty) {
                    Ok(expr) => expr,
                    Err(_) => quote! { ::std::string::String::from("any") },
                }
            };
            ts_field_pairs.push((field_name_str.clone(), ts_type_expr));

            // JSON Schema — when there's a BAML type override we don't know the
            // actual JSON structure, so fall back to `{}` (any schema).
            // Otherwise auto-resolve; fall back to `{}` on failure.
            let schema_expr = if field_attrs.type_override.is_some() {
                quote! { ::serde_json::json!({}) }
            } else if field_attrs.vec_or_one {
                match resolve_schema_tokens_for_vec_or_one_field(&field.ty) {
                    Ok(expr) => expr,
                    Err(_) => quote! { ::serde_json::json!({}) },
                }
            } else {
                match resolve_schema_tokens(&field.ty) {
                    Ok(expr) => expr,
                    Err(_) => quote! { ::serde_json::json!({}) },
                }
            };
            let is_required = !is_option_type(&field.ty);
            schema_field_triples.push((field_name_str.clone(), schema_expr, is_required));
        }

        field_tokens.push(quote! {
            ::baml_derive_core::BamlFieldDef {
                name: #field_name_str,
                baml_type: #baml_type_expr,
                alias: #alias_tokens,
                description: #desc_tokens,
                skip: #skip,
            }
        });
    }

    let dep_tokens = dep_names.iter().map(|d| quote! { #d }).collect::<Vec<_>>();
    let ts_dep_tokens = ts_dep_names
        .iter()
        .map(|d| quote! { #d })
        .collect::<Vec<_>>();

    // TypeScript: build `export interface` body.
    let ts_field_stmts: Vec<TokenStream> = ts_field_pairs
        .iter()
        .map(|(fname, ts_expr)| {
            quote! {
                out.push_str(&::std::format!("  {}: {};\n", #fname, #ts_expr));
            }
        })
        .collect();
    let ts_header = format!("export interface {name_str} {{\n");

    // JSON Schema: build properties insertions and required list.
    let schema_prop_stmts: Vec<TokenStream> = schema_field_triples
        .iter()
        .map(|(fname, schema_expr, _)| {
            quote! {
                __props.insert(#fname.to_string(), #schema_expr);
            }
        })
        .collect();
    let schema_required_stmts: Vec<TokenStream> = schema_field_triples
        .iter()
        .filter(|(_, _, req)| *req)
        .map(|(fname, _, _)| {
            quote! {
                __required.push(::serde_json::Value::String(#fname.to_string()));
            }
        })
        .collect();
    let title_str = name_str.clone();

    Ok(quote! {
        impl ::baml_derive_core::BamlType for #name {
            fn baml_type_name() -> &'static str {
                #name_str
            }

            fn baml_definition() -> ::baml_derive_core::BamlDefinition {
                ::baml_derive_core::BamlDefinition::Class(::baml_derive_core::BamlClassDef {
                    name: #name_str,
                    doc: #doc_tokens,
                    fields: ::std::vec![#(#field_tokens),*],
                    dynamic: #dynamic,
                })
            }

            fn baml_dependencies() -> ::std::vec::Vec<&'static str> {
                ::std::vec![#(#dep_tokens),*]
            }
        }

        impl ::baml_derive_core::TsType for #name {
            fn ts_type_name() -> &'static str {
                #name_str
            }

            fn ts_decl() -> ::std::option::Option<::std::string::String> {
                let mut out = ::std::string::String::from(#ts_header);
                #(#ts_field_stmts)*
                out.push('}');
                ::std::option::Option::Some(out)
            }

            fn ts_dependencies() -> ::std::vec::Vec<&'static str> {
                ::std::vec![#(#ts_dep_tokens),*]
            }
        }

        impl ::baml_derive_core::JsonSchemaType for #name {
            fn json_schema_inline() -> ::serde_json::Value {
                let mut __props = ::serde_json::Map::new();
                #(#schema_prop_stmts)*
                let mut __required: ::std::vec::Vec<::serde_json::Value> = ::std::vec::Vec::new();
                #(#schema_required_stmts)*
                let mut __schema = ::serde_json::Map::new();
                __schema.insert("type".to_string(), ::serde_json::Value::String("object".to_string()));
                __schema.insert("title".to_string(), ::serde_json::Value::String(#title_str.to_string()));
                __schema.insert("properties".to_string(), ::serde_json::Value::Object(__props));
                if !__required.is_empty() {
                    __schema.insert("required".to_string(), ::serde_json::Value::Array(__required));
                }
                ::serde_json::Value::Object(__schema)
            }
        }
    })
}

/// Expand for an enum with unit variants → `BamlDefinition::Enum`.
fn expand_enum(input: &DeriveInput, data: &DataEnum) -> Result<TokenStream, syn::Error> {
    let name = &input.ident;
    let name_str = name.to_string();

    let doc = extract_doc_comment(&input.attrs);
    let doc_tokens = option_str_tokens(&doc);

    let mut variant_tokens = Vec::new();
    // Collect non-skipped variant name strings for the TypeScript union.
    let mut ts_variant_names: Vec<String> = Vec::new();

    for variant in &data.variants {
        // Ensure all variants are unit variants for a regular BAML enum.
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                "BAML enums require unit variants; for enums with data, use `#[baml(union)]`",
            ));
        }

        let variant_attrs = parse_variant_attrs(&variant.attrs)?;
        let variant_name_str = variant.ident.to_string();
        let alias_tokens = option_str_tokens(&variant_attrs.alias);
        let desc_tokens = option_str_tokens(&variant_attrs.description);
        let skip = variant_attrs.skip;

        if !skip {
            ts_variant_names.push(variant_name_str.clone());
        }

        variant_tokens.push(quote! {
            ::baml_derive_core::BamlVariantDef {
                name: #variant_name_str,
                alias: #alias_tokens,
                description: #desc_tokens,
                skip: #skip,
            }
        });
    }

    // Build the TypeScript `export type Foo = "A" | "B" | ...;` expression.
    let ts_decl_expr = if ts_variant_names.is_empty() {
        // All variants skipped — emit `export type Foo = never;`
        let never_decl = format!("export type {name_str} = never;");
        quote! { ::std::option::Option::Some(::std::string::String::from(#never_decl)) }
    } else {
        let joined = ts_variant_names
            .iter()
            .map(|v| format!("\"{v}\""))
            .collect::<Vec<_>>()
            .join(" | ");
        let full_decl = format!("export type {name_str} = {joined};");
        quote! { ::std::option::Option::Some(::std::string::String::from(#full_decl)) }
    };

    Ok(quote! {
        impl ::baml_derive_core::BamlType for #name {
            fn baml_type_name() -> &'static str {
                #name_str
            }

            fn baml_definition() -> ::baml_derive_core::BamlDefinition {
                ::baml_derive_core::BamlDefinition::Enum(::baml_derive_core::BamlEnumDef {
                    name: #name_str,
                    doc: #doc_tokens,
                    variants: ::std::vec![#(#variant_tokens),*],
                })
            }
        }

        impl ::baml_derive_core::TsType for #name {
            fn ts_type_name() -> &'static str {
                #name_str
            }

            fn ts_decl() -> ::std::option::Option<::std::string::String> {
                #ts_decl_expr
            }
        }

        impl ::baml_derive_core::JsonSchemaType for #name {
            fn json_schema_inline() -> ::serde_json::Value {
                let __variants: ::std::vec::Vec<::serde_json::Value> = ::std::vec![
                    #(::serde_json::Value::String(#ts_variant_names.to_string())),*
                ];
                ::serde_json::json!({"type": "string", "enum": __variants})
            }
        }
    })
}

/// Expand for an enum with `#[baml(union)]` → `BamlDefinition::Union`.
///
/// Each variant must be a newtype variant wrapping a type that implements `BamlType`.
fn expand_union_enum(input: &DeriveInput, data: &DataEnum) -> Result<TokenStream, syn::Error> {
    let name = &input.ident;
    let name_str = name.to_string();

    let doc = extract_doc_comment(&input.attrs);
    let doc_tokens = option_str_tokens(&doc);

    let mut variant_type_names = Vec::new();
    let mut dep_names = Vec::new();

    for variant in &data.variants {
        match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                let inner_ty = &fields.unnamed[0].ty;
                let type_name = extract_type_name(inner_ty).ok_or_else(|| {
                    syn::Error::new_spanned(
                        inner_ty,
                        "union variant type must be a simple named type",
                    )
                })?;
                dep_names.push(type_name.clone());
                variant_type_names.push(type_name);
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "BAML union variants must be newtype variants, e.g. `Variant(InnerType)`",
                ));
            }
        }
    }

    let variant_str_tokens = variant_type_names
        .iter()
        .map(|n| quote! { #n })
        .collect::<Vec<_>>();

    let dep_tokens = dep_names.iter().map(|d| quote! { #d }).collect::<Vec<_>>();

    // Build `export type Foo = TypeA | TypeB | ...;`
    let ts_union = variant_type_names.join(" | ");
    let ts_decl_str = format!("export type {name_str} = {ts_union};");

    // Collect ident tokens for JsonSchemaType anyOf calls.
    let variant_ident_tokens: Vec<TokenStream> = variant_type_names
        .iter()
        .map(|n| {
            let ident =
                syn::parse_str::<syn::Ident>(n).expect("variant type name is a valid ident");
            quote! { <#ident as ::baml_derive_core::JsonSchemaType>::json_schema_inline() }
        })
        .collect();

    Ok(quote! {
        impl ::baml_derive_core::BamlType for #name {
            fn baml_type_name() -> &'static str {
                #name_str
            }

            fn baml_definition() -> ::baml_derive_core::BamlDefinition {
                ::baml_derive_core::BamlDefinition::Union(::baml_derive_core::BamlUnionDef {
                    name: #name_str,
                    doc: #doc_tokens,
                    variants: ::std::vec![#(#variant_str_tokens),*],
                })
            }

            fn baml_dependencies() -> ::std::vec::Vec<&'static str> {
                ::std::vec![#(#dep_tokens),*]
            }
        }

        impl ::baml_derive_core::TsType for #name {
            fn ts_type_name() -> &'static str {
                #name_str
            }

            fn ts_decl() -> ::std::option::Option<::std::string::String> {
                ::std::option::Option::Some(::std::string::String::from(#ts_decl_str))
            }

            fn ts_dependencies() -> ::std::vec::Vec<&'static str> {
                ::std::vec![#(#dep_tokens),*]
            }
        }

        impl ::baml_derive_core::JsonSchemaType for #name {
            fn json_schema_inline() -> ::serde_json::Value {
                let __any_of: ::std::vec::Vec<::serde_json::Value> =
                    ::std::vec![#(#variant_ident_tokens),*];
                ::serde_json::json!({"anyOf": __any_of})
            }
        }
    })
}

/// Helper: generate token stream for `Option<&'static str>`.
fn option_str_tokens(opt: &Option<String>) -> TokenStream {
    match opt {
        Some(s) => quote! { ::std::option::Option::Some(#s) },
        None => quote! { ::std::option::Option::None },
    }
}

/// Extract a simple type name from a `Type` (for dependency tracking and union variants).
///
/// Returns the last path segment's ident as a string, or `None` for complex types.
fn extract_type_name(ty: &Type) -> Option<String> {
    if let Type::Path(type_path) = ty {
        type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
    } else {
        None
    }
}

/// Extract user-type dependency name from a field type.
///
/// Returns `Some(name)` for types that are likely user-defined (not primitives,
/// not generic wrappers like Option/Vec). For generic wrappers, recurses into
/// the inner type.
fn extract_user_type_dep(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(type_path) => {
            let last = type_path.path.segments.last()?;
            let ident_str = last.ident.to_string();

            // Skip primitives
            if is_primitive(&ident_str) {
                return None;
            }

            // Recurse into known wrappers
            match ident_str.as_str() {
                "Option" | "Vec" | "Box" => {
                    let inner = extract_single_generic_type(last)?;
                    extract_user_type_dep(&inner)
                }
                "HashMap" | "BTreeMap" => {
                    // For maps, we'd need both key and value deps, but typically
                    // keys are primitives. For simplicity, check the value type.
                    let (_, val) = extract_two_generic_types(last)?;
                    extract_user_type_dep(&val)
                }
                _ if has_no_generics(last) => Some(ident_str),
                _ => None,
            }
        }
        Type::Reference(type_ref) => extract_user_type_dep(&type_ref.elem),
        _ => None,
    }
}

fn is_primitive(ident: &str) -> bool {
    matches!(
        ident,
        "String"
            | "str"
            | "bool"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "isize"
            | "usize"
            | "f32"
            | "f64"
    )
}

fn has_no_generics(segment: &syn::PathSegment) -> bool {
    matches!(segment.arguments, syn::PathArguments::None)
}

fn extract_single_generic_type(segment: &syn::PathSegment) -> Option<syn::Type> {
    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(ty)) = args.args.first()
    {
        return Some(ty.clone());
    }
    None
}

fn extract_two_generic_types(segment: &syn::PathSegment) -> Option<(syn::Type, syn::Type)> {
    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && args.args.len() >= 2
        && let (Some(syn::GenericArgument::Type(k)), Some(syn::GenericArgument::Type(v))) =
            (args.args.first(), args.args.iter().nth(1))
    {
        return Some((k.clone(), v.clone()));
    }
    None
}
