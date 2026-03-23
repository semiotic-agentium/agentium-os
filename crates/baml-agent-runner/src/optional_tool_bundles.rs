use std::sync::Arc;

use baml_rt_core::{AgentManifest, Result};
use baml_rt_provenance::ProvenanceOpsQuery;
use baml_rt_tools::tools::ToolRegistry;

/// Register optional host-managed tool bundles that require runtime context.
///
/// These initializers intentionally remain explicit business logic:
/// they may depend on manifest contents and contextual constructor args.
pub(crate) fn register_optional_tool_bundles(
    manifest: &AgentManifest,
    tool_registry: &ToolRegistry,
    provenance_query: Option<Arc<dyn ProvenanceOpsQuery>>,
) -> Result<()> {
    #[cfg(feature = "memory")]
    {
        if manifest.tools.iter().any(|t| t.starts_with("memory/")) {
            let memory_bundle = match &provenance_query {
                Some(pq) => {
                    baml_tools_memory::MemoryBundle::with_provenance(&manifest.name, pq.clone())?
                }
                None => baml_tools_memory::MemoryBundle::new(&manifest.name)?,
            };
            tool_registry.register_bundle(memory_bundle)?;
        }
    }
    #[cfg(not(feature = "memory"))]
    let _ = manifest;

    // Silence unused variable warning when memory feature is disabled
    let _ = provenance_query;

    Ok(())
}
