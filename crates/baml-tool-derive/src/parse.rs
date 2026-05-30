// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Attribute argument parsing for `#[baml_tool(...)]`.
//!
//! Parses the key-value and flag parameters from the macro invocation
//! into a structured [`ToolAttrs`] for code generation.

use proc_macro2::Span;
use syn::{
    Expr, ExprArray, ExprLit, ExprPath, Ident, Lit, LitStr, Path, Token, Type,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    spanned::Spanned,
};

/// A single secret requirement: `{ name = "...", description = "...", reason = "..." }`.
#[derive(Debug, Clone)]
pub(crate) struct SecretDef {
    pub name: String,
    pub description: String,
    pub reason: String,
}

/// Parsed config metadata from `config = { bundle = "...", schema = T, default = expr }`.
#[derive(Debug, Clone)]
pub(crate) struct ConfigDef {
    pub bundle: LitStr,
    pub schema: Type,
    pub default: Expr,
}

/// Parsed attributes from `#[baml_tool(...)]`.
#[derive(Debug)]
pub(crate) struct ToolAttrs {
    /// Qualified tool name, e.g. `"support/clickup"`.
    pub name: LitStr,
    /// Tool description for LLM consumption.
    pub description: LitStr,
    /// Tags for the tool (e.g. `["support", "clickup"]`).
    pub tags: Vec<LitStr>,
    /// Secret requirements.
    pub secrets: Vec<SecretDef>,
    /// Access level: `Read`, `Write`, or `Delete`.
    pub access: Option<Ident>,
    /// Types whose `baml_decl()` forms the BAML declaration.
    pub baml_types: Vec<Path>,
    /// Types whose `ts_decl()` produces extra TypeScript declarations.
    pub extra_ts_types: Vec<Path>,
    /// Event source kinds this tool can produce when polled (e.g. `["slack"]`).
    pub event_sources: Vec<LitStr>,
    /// Optional bundle config metadata for host-managed configuration.
    pub config: Option<ConfigDef>,
    /// Whether this is a metadata-only registration (no runtime handler).
    pub metadata_only: bool,
    /// Override for the default build function.
    pub build_with: Option<Path>,
    // Mode 2 (metadata_only) explicit types:
    /// The `OpenInput` type (required when `metadata_only` is set).
    pub open_input: Option<Type>,
    /// The `Input` type (required when `metadata_only` is set).
    pub input: Option<Type>,
    /// The `Output` type (required when `metadata_only` is set).
    pub output: Option<Type>,
}

/// Individual key-value or flag entry inside the attribute parentheses.
enum AttrEntry {
    Name(LitStr),
    Description(LitStr),
    Tags(Vec<LitStr>),
    Secrets(Vec<SecretDef>),
    Access(Ident),
    EventSources(Vec<LitStr>),
    Config(Box<ConfigDef>),
    BamlTypes(Vec<Path>),
    ExtraTsTypes(Vec<Path>),
    MetadataOnly,
    BuildWith(Path),
    OpenInput(Type),
    Input(Type),
    Output(Type),
}

enum ConfigEntry {
    Bundle(LitStr),
    Schema(Type),
    Default(Expr),
}

/// Parse the entire `(...)` contents of `#[baml_tool(...)]`.
impl Parse for ToolAttrs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let entries: Punctuated<AttrEntry, Token![,]> = Punctuated::parse_terminated(input)?;

        let mut name: Option<LitStr> = None;
        let mut description: Option<LitStr> = None;
        let mut tags: Vec<LitStr> = Vec::new();
        let mut secrets: Vec<SecretDef> = Vec::new();
        let mut access: Option<Ident> = None;
        let mut event_sources: Vec<LitStr> = Vec::new();
        let mut config: Option<ConfigDef> = None;
        let mut baml_types: Vec<Path> = Vec::new();
        let mut extra_ts_types: Vec<Path> = Vec::new();
        let mut metadata_only = false;
        let mut build_with: Option<Path> = None;
        let mut open_input: Option<Type> = None;
        let mut input_type: Option<Type> = None;
        let mut output: Option<Type> = None;

        for entry in entries {
            match entry {
                AttrEntry::Name(v) => name = Some(v),
                AttrEntry::Description(v) => description = Some(v),
                AttrEntry::Tags(v) => tags = v,
                AttrEntry::Secrets(v) => secrets = v,
                AttrEntry::Access(v) => access = Some(v),
                AttrEntry::EventSources(v) => event_sources = v,
                AttrEntry::Config(v) => config = Some(*v),
                AttrEntry::BamlTypes(v) => baml_types = v,
                AttrEntry::ExtraTsTypes(v) => extra_ts_types = v,
                AttrEntry::MetadataOnly => metadata_only = true,
                AttrEntry::BuildWith(v) => build_with = Some(v),
                AttrEntry::OpenInput(v) => open_input = Some(v),
                AttrEntry::Input(v) => input_type = Some(v),
                AttrEntry::Output(v) => output = Some(v),
            }
        }

        let name = name
            .ok_or_else(|| syn::Error::new(Span::call_site(), "baml_tool: `name` is required"))?;
        if name.value().is_empty() {
            return Err(syn::Error::new(
                name.span(),
                "baml_tool: `name` must not be empty",
            ));
        }
        let description = description.ok_or_else(|| {
            syn::Error::new(Span::call_site(), "baml_tool: `description` is required")
        })?;
        if description.value().is_empty() {
            return Err(syn::Error::new(
                description.span(),
                "baml_tool: `description` must not be empty",
            ));
        }

        Ok(ToolAttrs {
            name,
            description,
            tags,
            secrets,
            access,
            event_sources,
            config,
            baml_types,
            extra_ts_types,
            metadata_only,
            build_with,
            open_input,
            input: input_type,
            output,
        })
    }
}

impl ToolAttrs {
    /// Validate attribute consistency for Mode 1 (impl block).
    pub fn validate_mode1(&self) -> syn::Result<()> {
        if self.metadata_only {
            return Err(syn::Error::new(
                Span::call_site(),
                "baml_tool: `metadata_only` cannot be used on an `impl BamlTool` block; \
                 use it on a struct instead",
            ));
        }
        if self.open_input.is_some() || self.input.is_some() || self.output.is_some() {
            return Err(syn::Error::new(
                Span::call_site(),
                "baml_tool: `open_input`, `input`, and `output` are read from the \
                 `impl BamlTool` associated types; do not specify them in the attribute",
            ));
        }
        Ok(())
    }

    /// Validate attribute consistency for Mode 2 (struct / metadata-only).
    pub fn validate_mode2(&self) -> syn::Result<()> {
        if !self.metadata_only {
            return Err(syn::Error::new(
                Span::call_site(),
                "baml_tool: when applied to a struct, `metadata_only` must be set",
            ));
        }
        if self.build_with.is_some() {
            return Err(syn::Error::new(
                Span::call_site(),
                "baml_tool: `build_with` cannot be combined with `metadata_only`",
            ));
        }
        if self.open_input.is_none() {
            return Err(syn::Error::new(
                Span::call_site(),
                "baml_tool: `open_input` is required when `metadata_only` is set",
            ));
        }
        if self.input.is_none() {
            return Err(syn::Error::new(
                Span::call_site(),
                "baml_tool: `input` is required when `metadata_only` is set",
            ));
        }
        if self.output.is_none() {
            return Err(syn::Error::new(
                Span::call_site(),
                "baml_tool: `output` is required when `metadata_only` is set",
            ));
        }
        Ok(())
    }
}

/// Parse a single entry within the attribute arguments.
impl Parse for AttrEntry {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        // Check for bare flag: `metadata_only`
        if input.peek(Ident) && !input.peek2(Token![=]) {
            let ident: Ident = input.parse()?;
            if ident == "metadata_only" {
                return Ok(AttrEntry::MetadataOnly);
            }
            return Err(syn::Error::new(
                ident.span(),
                format!("baml_tool: unexpected flag `{ident}`; did you mean `metadata_only`?"),
            ));
        }

        let key: Ident = input.parse()?;
        input.parse::<Token![=]>()?;

        match key.to_string().as_str() {
            "name" => {
                let lit: LitStr = input.parse()?;
                Ok(AttrEntry::Name(lit))
            }
            "description" => {
                let lit: LitStr = input.parse()?;
                Ok(AttrEntry::Description(lit))
            }
            "tags" => {
                let strings = parse_string_array(input)?;
                Ok(AttrEntry::Tags(strings))
            }
            "secrets" => {
                let defs = parse_secrets_array(input)?;
                Ok(AttrEntry::Secrets(defs))
            }
            "access" => {
                let ident: Ident = input.parse()?;
                let valid = ["Read", "Write", "Delete"];
                if !valid.contains(&ident.to_string().as_str()) {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("baml_tool: `access` must be one of: {}", valid.join(", ")),
                    ));
                }
                Ok(AttrEntry::Access(ident))
            }
            "event_sources" => {
                let strings = parse_string_array(input)?;
                if strings.is_empty() {
                    return Err(syn::Error::new(
                        key.span(),
                        "baml_tool: `event_sources` must not be an empty array; omit it if the tool does not produce events",
                    ));
                }
                for s in &strings {
                    if s.value().trim().is_empty() {
                        return Err(syn::Error::new(
                            s.span(),
                            "baml_tool: `event_sources` entries must be non-empty strings",
                        ));
                    }
                }
                Ok(AttrEntry::EventSources(strings))
            }
            "config" => {
                let content;
                syn::braced!(content in input);
                let entries: Punctuated<ConfigEntry, Token![,]> =
                    Punctuated::parse_terminated(&content)?;

                let mut bundle: Option<LitStr> = None;
                let mut schema: Option<Type> = None;
                let mut default: Option<Expr> = None;

                for entry in entries {
                    match entry {
                        ConfigEntry::Bundle(v) => bundle = Some(v),
                        ConfigEntry::Schema(v) => schema = Some(v),
                        ConfigEntry::Default(v) => default = Some(v),
                    }
                }

                let bundle = bundle.ok_or_else(|| {
                    syn::Error::new(Span::call_site(), "baml_tool: `config.bundle` is required")
                })?;
                let schema = schema.ok_or_else(|| {
                    syn::Error::new(Span::call_site(), "baml_tool: `config.schema` is required")
                })?;
                let default = default.ok_or_else(|| {
                    syn::Error::new(Span::call_site(), "baml_tool: `config.default` is required")
                })?;

                Ok(AttrEntry::Config(Box::new(ConfigDef {
                    bundle,
                    schema,
                    default,
                })))
            }
            "baml_types" => {
                let paths = parse_path_array(input)?;
                Ok(AttrEntry::BamlTypes(paths))
            }
            "extra_ts_types" => {
                let paths = parse_path_array(input)?;
                Ok(AttrEntry::ExtraTsTypes(paths))
            }
            "build_with" => {
                let path: Path = input.parse()?;
                Ok(AttrEntry::BuildWith(path))
            }
            "open_input" => {
                let ty: Type = input.parse()?;
                Ok(AttrEntry::OpenInput(ty))
            }
            "input" => {
                let ty: Type = input.parse()?;
                Ok(AttrEntry::Input(ty))
            }
            "output" => {
                let ty: Type = input.parse()?;
                Ok(AttrEntry::Output(ty))
            }
            other => Err(syn::Error::new(
                key.span(),
                format!("baml_tool: unknown attribute `{other}`"),
            )),
        }
    }
}

impl Parse for ConfigEntry {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        input.parse::<Token![=]>()?;

        match key.to_string().as_str() {
            "bundle" => Ok(ConfigEntry::Bundle(input.parse()?)),
            "schema" => Ok(ConfigEntry::Schema(input.parse()?)),
            "default" => Ok(ConfigEntry::Default(input.parse()?)),
            other => Err(syn::Error::new(
                key.span(),
                format!(
                    "baml_tool: unknown config attribute `{other}`; expected bundle, schema, or default"
                ),
            )),
        }
    }
}

/// Parse `["a", "b", "c"]` into a `Vec<LitStr>`.
fn parse_string_array(input: ParseStream<'_>) -> syn::Result<Vec<LitStr>> {
    let expr: Expr = input.parse()?;
    let Expr::Array(ExprArray { elems, .. }) = expr else {
        return Err(syn::Error::new(
            expr.span(),
            "expected an array literal, e.g. [\"a\", \"b\"]",
        ));
    };
    let mut out = Vec::with_capacity(elems.len());
    for elem in elems {
        let Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) = elem
        else {
            return Err(syn::Error::new(
                elem.span(),
                "expected a string literal inside the array",
            ));
        };
        out.push(s);
    }
    Ok(out)
}

/// Parse `[TypeA, TypeB]` into a `Vec<Path>`.
fn parse_path_array(input: ParseStream<'_>) -> syn::Result<Vec<Path>> {
    let expr: Expr = input.parse()?;
    let Expr::Array(ExprArray { elems, .. }) = expr else {
        return Err(syn::Error::new(
            expr.span(),
            "expected an array of type paths, e.g. [MyType, OtherType]",
        ));
    };
    let mut out = Vec::with_capacity(elems.len());
    for elem in elems {
        let Expr::Path(ExprPath { path, .. }) = elem else {
            return Err(syn::Error::new(
                elem.span(),
                "expected a type path inside the array",
            ));
        };
        out.push(path);
    }
    Ok(out)
}

/// Parse `[{ name = "...", description = "...", reason = "..." }, ...]`.
fn parse_secrets_array(input: ParseStream<'_>) -> syn::Result<Vec<SecretDef>> {
    let content;
    syn::bracketed!(content in input);
    let entries: Punctuated<SecretDef, Token![,]> = Punctuated::parse_terminated(&content)?;
    Ok(entries.into_iter().collect())
}

/// Parse a single `{ name = "...", description = "...", reason = "..." }`.
impl Parse for SecretDef {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let content;
        syn::braced!(content in input);

        let mut name: Option<String> = None;
        let mut description: Option<String> = None;
        let mut reason: Option<String> = None;

        let entries: Punctuated<KvPair, Token![,]> = Punctuated::parse_terminated(&content)?;

        for KvPair(key, value) in entries {
            match key.to_string().as_str() {
                "name" => name = Some(value.value()),
                "description" => description = Some(value.value()),
                "reason" => reason = Some(value.value()),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "baml_tool: unknown secret field `{other}`; expected name, description, or reason"
                        ),
                    ));
                }
            }
        }

        Ok(SecretDef {
            name: name.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "baml_tool: secret `name` is required")
            })?,
            description: description.ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    "baml_tool: secret `description` is required",
                )
            })?,
            reason: reason.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "baml_tool: secret `reason` is required")
            })?,
        })
    }
}

/// A key-value pair: `key = "value"` (newtype to satisfy orphan rules).
struct KvPair(Ident, LitStr);

impl Parse for KvPair {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let value: LitStr = input.parse()?;
        Ok(KvPair(key, value))
    }
}

#[cfg(test)]
mod tests {
    use syn::parse_str;

    use super::ToolAttrs;

    #[test]
    fn event_sources_rejects_empty_array() {
        let err = parse_str::<ToolAttrs>(
            r#"name = "support/test", description = "test", event_sources = []"#,
        )
        .expect_err("empty event_sources should be rejected");

        assert!(err.to_string().contains("must not be an empty array"));
    }
}
