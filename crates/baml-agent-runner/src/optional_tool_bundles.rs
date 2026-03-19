use baml_rt_core::{AgentManifest, Result};
use baml_rt_tools::tools::ToolRegistry;
#[cfg(feature = "memory")]
use baml_tool_links::baml_tools_memory;

/// Register optional host-managed tool bundles that require runtime context.
///
/// These initializers intentionally remain explicit business logic:
/// they may depend on manifest contents and contextual constructor args.
pub(crate) fn register_optional_tool_bundles(
    manifest: &AgentManifest,
    tool_registry: &ToolRegistry,
) -> Result<()> {
    #[cfg(feature = "memory")]
    {
        if manifest.tools.iter().any(|t| t.starts_with("memory/")) {
            let memory_bundle = baml_tools_memory::MemoryBundle::new(&manifest.name)?;
            tool_registry.register_bundle(memory_bundle)?;
        }
    }

    Ok(())
}
