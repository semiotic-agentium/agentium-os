// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Code generation for `#[baml_tool]`.
//!
//! Handles expansion for:
//! - **Mode 1**: `#[baml_tool(...)] impl BamlTool for ToolStruct { ... }`
//!   Generates metadata fn + build fn + `register_tool!`.
//! - **Mode 2**: `#[baml_tool(..., metadata_only)] struct ToolName;`
//!   Generates metadata fn + unused build fn + `register_tool!`.

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use syn::{ImplItem, ItemImpl, ItemStruct, Path, Type};

use crate::parse::ToolAttrs;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Derive a collision-free function name prefix from a tool name string.
///
/// `"support/clickup"` → `support_clickup`
/// `"memory/add"` → `memory_add`
fn fn_prefix_from_tool_name(name: &str) -> String {
    name.replace(['/', '-'], "_")
}

/// Emit the `with_tags(...)` builder call (or nothing if empty).
fn tags_tokens(tags: &[syn::LitStr]) -> TokenStream {
    if tags.is_empty() {
        return TokenStream::new();
    }
    let tag_values = tags.iter().map(|t| {
        quote! { ::std::string::String::from(#t) }
    });
    quote! {
        .with_tags(::std::vec![#(#tag_values),*])
    }
}

/// Emit the `with_secret_requests(...)` builder call.
/// SecretDef.reason → justification, SecretDef.description → descriptor.
fn secrets_tokens(secrets: &[crate::parse::SecretDef]) -> TokenStream {
    if secrets.is_empty() {
        return TokenStream::new();
    }
    let values = secrets.iter().map(|s| {
        let name = &s.name;
        let justification = &s.reason;
        let descriptor = &s.description;
        quote! {
            ::baml_rt_tools::tools::SecretRequest::api_key(#name, #justification, #descriptor)
        }
    });
    quote! {
        .with_secret_requests(::std::vec![#(#values),*])
    }
}

/// Emit the `with_access(...)` builder call (or nothing if not set).
fn access_tokens(access: &Option<Ident>) -> TokenStream {
    let Some(ident) = access else {
        return TokenStream::new();
    };
    let variant = format_ident!("{}", ident);
    quote! {
        .with_access(::baml_rt_tools::tools::ToolAccess::#variant)
    }
}

/// Emit the `with_event_sources(...)` builder call (or nothing if empty).
///
/// Each string is validated at registration time via `EventSourceKind::parse`.
fn event_sources_tokens(sources: &[syn::LitStr]) -> TokenStream {
    if sources.is_empty() {
        return TokenStream::new();
    }
    let items = sources.iter().map(|s| {
        quote! {
            ::baml_rt_core::EventSourceKind::parse(#s).expect(
                "baml_tool: invalid `event_sources` entry; must be a valid event source kind",
            )
        }
    });
    quote! {
        .with_event_sources(::std::vec![#(#items),*])
    }
}

/// Emit the `with_config_bundle(...)` + `with_config(...)` builder calls.
fn config_tokens(config: &Option<crate::parse::ConfigDef>) -> TokenStream {
    let Some(config) = config else {
        return TokenStream::new();
    };
    let bundle = &config.bundle;
    let schema_ty = &config.schema;
    let default_expr = &config.default;
    quote! {
        .with_config_bundle(
            ::baml_rt_tools::BundleName::new(#bundle)
                .expect("baml_tool: config bundle must be a valid bundle name"),
        )
        .with_config(::baml_rt_tools::ToolConfigMetadata::new(
            ::baml_rt_tools::json_schema_value::<#schema_ty>(),
            ::serde_json::to_value(#default_expr)
                .expect("baml_tool: failed to serialize config default"),
            ::core::option::Option::Some(::baml_rt_tools::ts_name::<#schema_ty>()),
        ))
    }
}

/// Emit the `with_extra_ts_decls(...)` builder call (or nothing if empty).
///
/// Uses `TsType::ts_decl()` via `baml_rt_tools::ts_decl` which now delegates
/// to the `TsType` trait from `baml-derive-core`.
fn extra_ts_decls_tokens(extra_ts_types: &[Path]) -> TokenStream {
    if extra_ts_types.is_empty() {
        return TokenStream::new();
    }
    let decl_calls = extra_ts_types.iter().map(|ty| {
        quote! {
            <#ty as ::baml_derive_core::TsType>::ts_decl()
        }
    });
    quote! {
        .with_extra_ts_decls(
            [#(#decl_calls),*].into_iter().flatten().collect()
        )
    }
}

/// Emit the `with_baml_decl(...)` builder call (or nothing if no baml_types).
fn baml_decl_tokens(baml_types: &[Path]) -> TokenStream {
    if baml_types.is_empty() {
        return TokenStream::new();
    }
    let decl_calls = baml_types.iter().map(|ty| {
        quote! {
            <#ty as ::baml_derive_core::BamlType>::baml_decl()
        }
    });
    quote! {
        .with_baml_decl(
            [#(#decl_calls),*].join("\n\n")
        )
    }
}

// ---------------------------------------------------------------------------
// Mode 1: #[baml_tool(...)] impl BamlTool for ToolStruct { ... }
// ---------------------------------------------------------------------------

/// Extract a named associated type from an impl block.
///
/// Looks for `type Name = SomeType;` items and returns the RHS type.
fn extract_associated_type(impl_block: &ItemImpl, name: &str) -> syn::Result<Type> {
    for item in &impl_block.items {
        if let ImplItem::Type(assoc) = item
            && assoc.ident == name
        {
            return Ok(assoc.ty.clone());
        }
    }
    Err(syn::Error::new_spanned(
        impl_block,
        format!(
            "baml_tool: could not find associated type `{name}` in the impl block; \
             make sure `type {name} = ...;` is defined"
        ),
    ))
}

/// Extract the implementing struct type from `impl Trait for StructType`.
fn extract_self_type(impl_block: &ItemImpl) -> syn::Result<&Type> {
    Ok(&*impl_block.self_ty)
}

/// Expand Mode 1: `#[baml_tool(...)] impl BamlTool for ToolStruct { ... }`.
pub(crate) fn expand_impl(attrs: &ToolAttrs, impl_block: &ItemImpl) -> syn::Result<TokenStream> {
    attrs.validate_mode1()?;

    let open_input_ty = extract_associated_type(impl_block, "OpenInput")?;
    let input_ty = extract_associated_type(impl_block, "Input")?;
    let output_ty = extract_associated_type(impl_block, "Output")?;
    let self_ty = extract_self_type(impl_block)?;

    let tool_name = &attrs.name;
    let description = &attrs.description;
    let prefix = fn_prefix_from_tool_name(&tool_name.value());
    let metadata_fn = format_ident!("{}_metadata", prefix);
    let build_fn = format_ident!("{}_build", prefix);

    let tags = tags_tokens(&attrs.tags);
    let secrets = secrets_tokens(&attrs.secrets);
    let access = access_tokens(&attrs.access);
    let event_sources = event_sources_tokens(&attrs.event_sources);
    let config = config_tokens(&attrs.config);
    let baml_decl = baml_decl_tokens(&attrs.baml_types);
    let extra_ts = extra_ts_decls_tokens(&attrs.extra_ts_types);

    let build_body = if let Some(ref custom_build) = attrs.build_with {
        quote! { #custom_build() }
    } else {
        quote! {
            ::baml_rt_tools::tools::create_tool_handler(
                <#self_ty as ::std::default::Default>::default()
            ).map(|(_, h)| h)
        }
    };

    let expect_msg = format!("{} is a compile-time constant", tool_name.value());

    Ok(quote! {
        // Pass through the original impl block unchanged.
        #impl_block

        // --- Generated by #[baml_tool] ---

        pub fn #metadata_fn() -> ::baml_rt_tools::tools::ToolFunctionMetadata {
            use ::baml_rt_tools::ToolMetadataBuilder as _;
            let (name, class_name) = ::baml_rt_tools::parse_tool_name_and_class(#tool_name)
                .expect(#expect_msg);
            ::baml_rt_tools::TypeBasedMetadataBuilder::<#open_input_ty, #input_ty, #output_ty>::new(
                name,
                class_name,
                ::std::string::String::from(#description),
            )
            #baml_decl
            #extra_ts
            #tags
            #secrets
            #access
            #config
            #event_sources
            .build_metadata()
        }

        pub fn #build_fn() -> ::baml_rt_core::Result<::std::sync::Arc<dyn ::baml_rt_tools::tools::ToolHandler>> {
            #build_body
        }

        ::baml_rt_tools::register_tool!(#metadata_fn, #build_fn);
    })
}

// ---------------------------------------------------------------------------
// Mode 2:
// ---------------------------------------------------------------------------

/// Expand Mode 2: `#[baml_tool(..., metadata_only)] struct ToolName;`.
pub(crate) fn expand_struct(attrs: &ToolAttrs, item: &ItemStruct) -> syn::Result<TokenStream> {
    attrs.validate_mode2()?;

    // Safe to unwrap — validate_mode2 checks these are Some.
    let open_input_ty = attrs
        .open_input
        .as_ref()
        .expect("validated in validate_mode2");
    let input_ty = attrs.input.as_ref().expect("validated in validate_mode2");
    let output_ty = attrs.output.as_ref().expect("validated in validate_mode2");

    let tool_name = &attrs.name;
    let description = &attrs.description;
    let prefix = fn_prefix_from_tool_name(&tool_name.value());
    let metadata_fn = format_ident!("{}_metadata", prefix);
    let build_fn = format_ident!("{}_build", prefix);

    let tags = tags_tokens(&attrs.tags);
    let secrets = secrets_tokens(&attrs.secrets);
    let access = access_tokens(&attrs.access);
    let event_sources = event_sources_tokens(&attrs.event_sources);
    let config = config_tokens(&attrs.config);
    let baml_decl = baml_decl_tokens(&attrs.baml_types);
    let extra_ts = extra_ts_decls_tokens(&attrs.extra_ts_types);

    let expect_msg = format!("{} is a compile-time constant", tool_name.value());
    let err_msg = format!(
        "{} is a metadata-only tool; runtime handler is provided by the host bundle",
        tool_name.value(),
    );

    Ok(quote! {
        // Pass through the original struct unchanged.
        #item

        // --- Generated by #[baml_tool] (metadata_only) ---

        pub fn #metadata_fn() -> ::baml_rt_tools::tools::ToolFunctionMetadata {
            use ::baml_rt_tools::ToolMetadataBuilder as _;
            let (name, class_name) = ::baml_rt_tools::parse_tool_name_and_class(#tool_name)
                .expect(#expect_msg);
            ::baml_rt_tools::TypeBasedMetadataBuilder::<#open_input_ty, #input_ty, #output_ty>::new(
                name,
                class_name,
                ::std::string::String::from(#description),
            )
            #baml_decl
            #extra_ts
            #tags
            #secrets
            #access
            #config
            #event_sources
            .build_metadata()
        }

        pub fn #build_fn() -> ::baml_rt_core::Result<::std::sync::Arc<dyn ::baml_rt_tools::tools::ToolHandler>> {
            ::std::result::Result::Err(
                ::baml_rt_core::BamlRtError::InvalidArgument(
                    ::std::string::String::from(#err_msg)
                )
            )
        }

        ::baml_rt_tools::register_tool!(#metadata_fn, #build_fn);
    })
}
