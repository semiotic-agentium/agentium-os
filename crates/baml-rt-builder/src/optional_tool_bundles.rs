use baml_rt_core::Result;
use baml_rt_tools::tools::ToolRegistry;
#[cfg(feature = "memory")]
use baml_tool_links::baml_tools_memory;

/// Register optional host-managed tool bundles required for builder runtime flows.
pub(crate) fn register_optional_tool_bundles(
    agent_name: &str,
    manifest_tools: &[String],
    tool_registry: &ToolRegistry,
) -> Result<()> {
    #[cfg(feature = "memory")]
    {
        if manifest_tools.iter().any(|t| t.starts_with("memory/")) {
            let memory_bundle = baml_tools_memory::MemoryBundle::new(agent_name)?;
            tool_registry.register_bundle(memory_bundle)?;
        }
    }

    Ok(())
}
