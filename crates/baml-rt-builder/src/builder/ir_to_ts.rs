// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Map BAML IR types (TypeNonStreaming) to TypeScript type expressions.
//!
//! Semantics aligned with upstream BoundaryML TS generator: primitives, optional
//! as T | null, unions, classes, enums, lists, maps. No BAML source parsing.

use std::collections::HashSet;

use baml_types::ir_type::{LiteralValue, TypeNonStreaming, TypeValue, UnionTypeViewGeneric};
use genco::{lang::js, prelude::*};
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::error::{BamlBuilderError, Result};

/// Result of mapping one IR type: TS type expression and any type names that must be declared.
#[derive(Debug, Clone)]
pub struct TsTypeFrag {
    pub expr: String,
    pub deps: Vec<String>,
}

/// Map a non-streaming BAML type to a TypeScript type expression and collect referenced type names.
pub fn type_to_ts_expr(ty: &TypeNonStreaming, _ir: &IRSignature) -> Result<TsTypeFrag> {
    let mut deps = Vec::new();
    let expr = type_to_ts_inner(ty, &mut deps)?;
    Ok(TsTypeFrag { expr, deps })
}

fn type_to_ts_inner(ty: &TypeNonStreaming, deps: &mut Vec<String>) -> Result<String> {
    use TypeNonStreaming as T;

    let mut recursive = |t: &TypeNonStreaming| -> Result<String> { type_to_ts_inner(t, deps) };

    Ok(match ty {
        T::Primitive(tv, _) => match tv {
            TypeValue::String => "string".to_string(),
            TypeValue::Int | TypeValue::Float => "number".to_string(),
            TypeValue::Bool => "boolean".to_string(),
            TypeValue::Null => "null".to_string(),
            TypeValue::Media(_) => "string".to_string(), // media as string in TS surface
        },
        T::Enum { name, .. } => {
            deps.push(name.clone());
            name.clone()
        }
        T::Literal(lit, _) => match lit {
            LiteralValue::String(s) => format!("{:?}", s),
            LiteralValue::Int(n) => n.to_string(),
            LiteralValue::Bool(b) => b.to_string(),
        },
        T::Class { name, .. } => {
            deps.push(name.clone());
            name.clone()
        }
        T::List(inner, _) => {
            let inner_ts = recursive(inner)?;
            // `A | B[]` parses as `A | (B[])` — parenthesize union element types.
            if inner_ts.contains(" | ") {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        T::Map(_k, v, _) => {
            let v_ts = recursive(v)?;
            format!("Record<string, {v_ts}>")
        }
        T::RecursiveTypeAlias { name, .. } => {
            deps.push(name.clone());
            name.clone()
        }
        T::Tuple(inner, _) => {
            let parts: Vec<String> = inner.iter().map(&mut recursive).collect::<Result<_>>()?;
            format!("[{}]", parts.join(", "))
        }
        T::Arrow(..) => {
            return Err(BamlBuilderError::InvalidArgument(
                "Arrow types are not supported in generated TypeScript".to_string(),
            ));
        }
        T::Union(union_gen, _) => match union_gen.view() {
            UnionTypeViewGeneric::Null => "null".to_string(),
            UnionTypeViewGeneric::Optional(inner) => {
                let inner_ts = recursive(inner)?;
                format!("{inner_ts} | null")
            }
            UnionTypeViewGeneric::OneOf(variants) => {
                let parts: Vec<String> = variants
                    .iter()
                    .map(|t| recursive(t))
                    .collect::<Result<_>>()?;
                parts.join(" | ")
            }
            UnionTypeViewGeneric::OneOfOptional(variants) => {
                let parts: Vec<String> = variants
                    .iter()
                    .map(|t| recursive(t))
                    .collect::<Result<_>>()?;
                format!("{} | null", parts.join(" | "))
            }
        },
        T::Top(_) => {
            return Err(BamlBuilderError::InvalidArgument(
                "Top/any type should not appear in TS generation".to_string(),
            ));
        }
    })
}

/// Collect all type names that need declarations (classes, enums, type aliases) from a fragment's deps.
pub fn collect_type_decl_deps(frag: &TsTypeFrag) -> HashSet<String> {
    frag.deps.iter().cloned().collect()
}

/// Types defined in the bootstrap `render_tool_typescript` block (`baml_rt_tools::ts_gen`).
/// BAML IR also contains copies from `_baml_runtime.baml`; emitting both breaks TS (duplicate
/// `StructuredReply` and wrong `ReplyPart[]` precedence as `TextPart | DataPart[]`).
const BOOTSTRAP_TS_RUNTIME_TYPES: &[&str] = &[
    "StructuredReply",
    "TextPart",
    "DataPart",
    "ReplyPart",
    "ReplyMediaType",
];

/// Emit TypeScript type declarations (interfaces, enums, type aliases) for the given set of type names.
///
/// Transitively closes over:
/// - **class** fields (nested classes, enums, aliases in field types)
/// - **type alias** bodies (`type A = B | C` pulls in `B`, `C`, and their fields)
///
/// so TS never references a BAML class/enum/alias name without a matching `export`.
pub fn emit_type_declarations_tokens(
    ir: &IRSignature,
    needed: &HashSet<String>,
) -> Result<js::Tokens> {
    let mut all_needed = needed.clone();
    let mut worklist: Vec<String> = needed.iter().cloned().collect();
    while let Some(name) = worklist.pop() {
        if let Some((_, class_details)) = ir.classes.get(&name) {
            for (_, fty) in class_details.fields.iter() {
                let frag = type_to_ts_expr(fty.as_ref(), ir)?;
                for dep in frag.deps {
                    if all_needed.insert(dep.clone()) {
                        worklist.push(dep);
                    }
                }
            }
        } else if let Some(alias_node) = ir.type_aliases.get(&name) {
            // Return/arg types often surface as a `RecursiveTypeAlias` name only; the nested classes
            // live in the alias RHS and were previously never pulled into `all_needed`.
            let frag = type_to_ts_expr(alias_node.field_type.as_ref(), ir)?;
            for dep in frag.deps {
                if all_needed.insert(dep.clone()) {
                    worklist.push(dep);
                }
            }
        }
    }
    let mut names: Vec<&String> = all_needed.iter().collect();
    names.sort();
    let mut out: js::Tokens = quote!();
    for name in names {
        if BOOTSTRAP_TS_RUNTIME_TYPES.contains(&name.as_str()) {
            continue;
        }
        if let Some((_, class_details)) = ir.classes.get(name) {
            let mut fields: js::Tokens = quote!();
            for (fname, fty) in class_details.fields.iter() {
                let frag = type_to_ts_expr(fty.as_ref(), ir)?;
                let type_expr = frag.expr.as_str();
                quote_in!(fields => $(fname): $(type_expr););
                fields.push();
            }
            quote_in!(out => export interface $(name) { $(fields) });
            out.line();
        } else if let Some((_, enum_details)) = ir.enums.get(name) {
            let variants: Vec<String> = enum_details
                .values
                .iter()
                .map(|v| {
                    let escaped = v.replace('\\', "\\\\").replace('\"', "\\\"");
                    format!("\"{escaped}\"")
                })
                .collect();
            let union_expr = variants.join(" | ");
            quote_in!(out => export type $(name) = $(union_expr););
            out.line();
        } else if let Some(type_node) = ir.type_aliases.get(name) {
            let type_expr = if name == "OpaqueJson" {
                "JsonValue".to_string()
            } else {
                let frag = type_to_ts_expr(type_node.field_type.as_ref(), ir)?;
                frag.expr
            };
            quote_in!(out => export type $(name) = $(type_expr););
            out.line();
        }
    }
    Ok(out)
}
