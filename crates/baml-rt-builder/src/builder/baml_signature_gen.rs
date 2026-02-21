//! Harvest BAML function signatures from runtime IR for TypeScript declaration generation.
//!
//! Uses the loaded BAML runtime's IntermediateRepr (via IRSignature) as the sole type
//! authority; no BAML source text parsing.

use std::ops::Deref;

use baml_types::ir_type::{TypeGeneric, UnionTypeViewGeneric};
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::error::{BamlBuilderError, Result};

/// Extract full function signatures (name, inputs, output) from a loaded BAML runtime.
///
/// The runtime must already be loaded from the agent's baml_src (e.g. via
/// `BamlRuntime::from_directory`). Returns an `IRSignature` whose `functions` map
/// contains per-function input/output types for TS codegen.
pub fn extract_baml_signatures(runtime: &baml_runtime::BamlRuntime) -> Result<IRSignature> {
    let ir = runtime.ir.deref();
    IRSignature::new_from_ir(ir)
        .map_err(|source| BamlBuilderError::IrSignatureExtraction { source })
}

/// If the type is (or wraps) a class/alias whose name ends with "SessionPlan", return that name.
/// Works with `TypeGeneric<T>` so we support whatever meta type the IR uses (e.g. non_streaming::TypeMeta).
fn session_plan_type_name_from_generic<T>(ty: &TypeGeneric<T>) -> Option<String> {
    match ty {
        TypeGeneric::Class { name, .. } | TypeGeneric::RecursiveTypeAlias { name, .. } => {
            if name.ends_with("SessionPlan") {
                Some(name.clone())
            } else {
                None
            }
        }
        TypeGeneric::Union(union_gen, _) => match union_gen.view() {
            UnionTypeViewGeneric::Optional(inner) => session_plan_type_name_from_generic(inner),
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => variants
                .iter()
                .find_map(|variant| session_plan_type_name_from_generic(variant)),
            _ => None,
        },
        _ => None,
    }
}

/// Build a map from BAML function name to session plan type name for every function whose
/// return type is (or wraps) a class ending with "SessionPlan". The runtime uses this to
/// resolve the tool from the call site (source_baml_function) without requiring __type in the JSON.
///
/// Handles both class-based session plans (`class XSessionPlan { steps ... }`) detected
/// directly in the IR, and type-alias session plans (`type XSessionPlan = XSessionStep[]`)
/// which the IR inlines to `List(Union(...))`. The latter are matched by comparing the
/// function's expanded output type against known SessionPlan alias expansions.
pub fn session_plan_functions_map(ir: &IRSignature) -> std::collections::HashMap<String, String> {
    // Pre-compute canonical representations for type aliases ending in "SessionPlan".
    // These aliases get inlined in function output types, so we reverse the expansion here.
    let plan_alias_lookup: std::collections::HashMap<String, String> = ir
        .type_aliases
        .iter()
        .filter(|(name, _)| name.ends_with("SessionPlan"))
        .map(|(name, type_node)| {
            (
                canonical_type_key(type_node.field_type.as_ref()),
                name.clone(),
            )
        })
        .collect();

    let mut map = std::collections::HashMap::new();
    for (name, func_sig) in &ir.functions {
        // Direct match: Class or RecursiveTypeAlias with "SessionPlan" suffix.
        if let Some(plan_type) = session_plan_type_name_from_generic(&func_sig.output) {
            map.insert(name.clone(), plan_type);
            continue;
        }
        // Fallback: the output type may be an inlined type alias. Search the type tree
        // (including inside union variants) for a sub-type whose canonical key matches
        // a known SessionPlan alias expansion.
        if let Some(alias_name) = find_plan_alias_in_type(&func_sig.output, &plan_alias_lookup) {
            map.insert(name.clone(), alias_name);
        }
    }
    map
}

/// Search a type tree for a sub-type whose canonical key matches a known SessionPlan alias.
///
/// Recurses into union variants and optionals so that return types like
/// `FinalResponse | SupportClickupSessionPlan` (which the IR inlines to
/// `Union(Class("FinalResponse"), List(Union(...)))`) can still be matched.
fn find_plan_alias_in_type<T>(
    ty: &TypeGeneric<T>,
    plan_alias_lookup: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let key = canonical_type_key(ty);
    if let Some(alias_name) = plan_alias_lookup.get(&key) {
        return Some(alias_name.clone());
    }
    match ty {
        TypeGeneric::Union(union_gen, _) => match union_gen.view() {
            UnionTypeViewGeneric::Optional(inner) => {
                find_plan_alias_in_type(inner, plan_alias_lookup)
            }
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => variants
                .iter()
                .find_map(|variant| find_plan_alias_in_type(variant, plan_alias_lookup)),
            _ => None,
        },
        _ => None,
    }
}

/// Metadata-ignoring canonical representation of a type for structural comparison.
///
/// Two types that differ only in IR metadata (source positions, constraints) produce
/// the same key. Used to match inlined type alias expansions against their definitions.
fn canonical_type_key<T>(ty: &TypeGeneric<T>) -> String {
    match ty {
        TypeGeneric::Top(_) => "top".into(),
        TypeGeneric::Primitive(tv, _) => format!("prim:{tv:?}"),
        TypeGeneric::Enum { name, .. } => format!("enum:{name}"),
        TypeGeneric::Literal(lit, _) => format!("lit:{lit:?}"),
        TypeGeneric::Class { name, .. } => format!("class:{name}"),
        TypeGeneric::RecursiveTypeAlias { name, .. } => format!("alias:{name}"),
        TypeGeneric::List(inner, _) => format!("[{}]", canonical_type_key(inner)),
        TypeGeneric::Map(k, v, _) => {
            format!("map<{},{}>", canonical_type_key(k), canonical_type_key(v))
        }
        TypeGeneric::Tuple(inner, _) => {
            let parts: Vec<String> = inner.iter().map(|t| canonical_type_key(t)).collect();
            format!("({})", parts.join(","))
        }
        TypeGeneric::Arrow(_, _) => "arrow".into(),
        TypeGeneric::Union(union_gen, _) => match union_gen.view() {
            UnionTypeViewGeneric::Null => "null".into(),
            UnionTypeViewGeneric::Optional(inner) => {
                format!("{}?", canonical_type_key(inner))
            }
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => {
                let parts: Vec<String> = variants.iter().map(|t| canonical_type_key(t)).collect();
                parts.join("|")
            }
        },
    }
}
