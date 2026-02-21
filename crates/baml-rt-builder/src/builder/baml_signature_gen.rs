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
pub fn session_plan_functions_map(ir: &IRSignature) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for (name, func_sig) in &ir.functions {
        if let Some(plan_type) = session_plan_type_name_from_generic(&func_sig.output) {
            map.insert(name.clone(), plan_type);
        }
    }
    map
}
