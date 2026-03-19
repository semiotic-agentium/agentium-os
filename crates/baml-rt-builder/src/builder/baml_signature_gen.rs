//! Harvest BAML function signatures from runtime IR for TypeScript declaration generation.
//!
//! Uses the loaded BAML runtime's IntermediateRepr (via IRSignature) as the sole type
//! authority; no BAML source text parsing.

use std::ops::Deref;

use baml_rt_tools::{
    FunctionPlanBinding, FunctionRole, SessionPlanFunctionsMap, SessionPlanTypeName,
};
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

/// Collect ALL session plan type names from a (possibly union) return type.
///
/// Returns every class/alias name ending with `"SessionPlan"` found at any depth.
/// For single-tool functions the result has length 1; for polymorphic functions (union
/// returns with multiple session plan types) the result has length >1.
///
/// Public alias so `baml_gen` can reuse this from the compiled IR.
pub fn session_plan_type_names_from_ir<T>(ty: &TypeGeneric<T>) -> Vec<SessionPlanTypeName>
where
    T: Clone + std::fmt::Debug,
{
    session_plan_type_names_from_generic(ty)
}

fn session_plan_type_names_from_generic<T>(ty: &TypeGeneric<T>) -> Vec<SessionPlanTypeName> {
    match ty {
        TypeGeneric::Class { name, .. } | TypeGeneric::RecursiveTypeAlias { name, .. } => {
            if name.ends_with("SessionPlan") {
                vec![SessionPlanTypeName::new(name.clone()).unwrap()]
            } else {
                vec![]
            }
        }
        TypeGeneric::Union(union_gen, _) => match union_gen.view() {
            UnionTypeViewGeneric::Optional(inner) => session_plan_type_names_from_generic(inner),
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => variants
                .iter()
                .flat_map(|v| session_plan_type_names_from_generic(v))
                .collect(),
            _ => vec![],
        },
        _ => vec![],
    }
}

/// Build a map from BAML function name to candidate session plan type names.
///
/// Every function whose return type is (or wraps) one or more classes ending with
/// `"SessionPlan"` gets an entry. Length 1 = single-tool. Length >1 = polymorphic Open.
pub fn session_plan_functions_map(ir: &IRSignature) -> SessionPlanFunctionsMap {
    let mut map = SessionPlanFunctionsMap::new();
    for (name, func_sig) in &ir.functions {
        let plan_types = session_plan_type_names_from_generic(&func_sig.output);
        if !plan_types.is_empty() {
            map.insert(name.clone(), plan_types);
        }
    }
    map
}

/// Enriched session plan manifest with role annotations for each function.
pub type SessionPlanManifest = std::collections::HashMap<String, FunctionPlanBinding>;

/// Build an enriched manifest that annotates each session-plan function with its role.
///
/// Root functions are user-authored (present in the raw IR). Generated phase functions
/// (`__select`, `__act__`, `__consume__`, `__continue__`) are annotated by suffix pattern.
pub fn session_plan_manifest(ir: &IRSignature) -> SessionPlanManifest {
    let mut manifest = SessionPlanManifest::new();

    for (name, func_sig) in &ir.functions {
        let plan_types = session_plan_type_names_from_generic(&func_sig.output);
        if plan_types.is_empty() {
            continue;
        }

        let role = infer_function_role(name);
        manifest.insert(name.clone(), FunctionPlanBinding { plan_types, role });
    }

    manifest
}

/// Infer the `FunctionRole` from naming convention.
///
/// Uses the same suffix patterns as `SessionTypeNames` — the naming convention
/// is the contract between builder codegen and runtime phase selection.
fn infer_function_role(name: &str) -> FunctionRole {
    if name.contains("__select") {
        FunctionRole::Select
    } else if name.contains("__act__") {
        FunctionRole::Act
    } else if name.contains("__consume__") {
        FunctionRole::Consume
    } else if name.contains("__continue__") {
        FunctionRole::Continue
    } else {
        FunctionRole::Root
    }
}
