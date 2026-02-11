//! Harvest BAML function signatures from runtime IR for TypeScript declaration generation.
//!
//! Uses the loaded BAML runtime's IntermediateRepr (via IRSignature) as the sole type
//! authority; no BAML source text parsing.

use baml_rt_core::{BamlRtError, Result};
use internal_baml_core::ir::ir_hasher::IRSignature;
use std::ops::Deref;

/// Extract full function signatures (name, inputs, output) from a loaded BAML runtime.
///
/// The runtime must already be loaded from the agent's baml_src (e.g. via
/// `BamlRuntime::from_directory`). Returns an `IRSignature` whose `functions` map
/// contains per-function `TypeNonStreaming` input/output types for TS codegen.
pub fn extract_baml_signatures(runtime: &baml_runtime::BamlRuntime) -> Result<IRSignature> {
    let ir = runtime.ir.deref();
    IRSignature::new_from_ir(ir).map_err(|e| {
        BamlRtError::InvalidArgument(format!("Failed to extract BAML IR signatures: {e}",))
    })
}
