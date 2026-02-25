//! Tool metadata registration for the memory bundle (register_tool! + TypeBasedMetadataBuilder).

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolHandler, parse_tool_name_and_class, register_tool,
    tools::{ToolAccess, ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};

use crate::types::*;

fn memory_tool_build_unused() -> Result<Arc<dyn ToolHandler>> {
    Err(BamlRtError::InvalidArgument(
        "Memory tools are registered by the host via MemoryBundle".to_string(),
    ))
}

fn build_memory_metadata<Open, SendInput, Next>(
    tool_name: &str,
    description: &str,
    access: ToolAccess,
) -> ToolFunctionMetadata
where
    Open: schemars::JsonSchema + ts_rs::TS + Send + Sync + 'static,
    SendInput: schemars::JsonSchema + ts_rs::TS + Send + Sync + 'static,
    Next: schemars::JsonSchema + ts_rs::TS + Send + Sync + 'static,
{
    let (name, class_name) =
        parse_tool_name_and_class(tool_name).expect("memory tool name is a compile-time constant");
    TypeBasedMetadataBuilder::<Open, SendInput, Next>::new(
        name,
        class_name,
        description.to_string(),
    )
    .with_access(access)
    .with_tags(vec!["memory".to_string()])
    .build_metadata()
}

pub fn memory_add_metadata() -> ToolFunctionMetadata {
    build_memory_metadata::<MemoryAddOpenInput, MemoryAddSendInput, MemoryAddNextOutput>(
        "memory/add",
        "Store cognitive events (facts, decisions, inferences, corrections, skills, episodes) \
         with optional edges. Returns the IDs of newly created nodes.",
        ToolAccess::Write,
    )
}

pub fn memory_search_metadata() -> ToolFunctionMetadata {
    build_memory_metadata::<MemorySearchOpenInput, MemorySearchSendInput, MemorySearchNextOutput>(
        "memory/search",
        "BM25 text search over stored memories. Returns ranked matches with content and metadata. \
         Use this to recall relevant knowledge before making decisions.",
        ToolAccess::Read,
    )
}

pub fn memory_traverse_metadata() -> ToolFunctionMetadata {
    build_memory_metadata::<
        MemoryTraverseOpenInput,
        MemoryTraverseSendInput,
        MemoryTraverseNextOutput,
    >(
        "memory/traverse",
        "Walk reasoning chains in the memory graph from a starting node. \
         Follow typed edges (caused_by, supports, contradicts, etc.) to explore how knowledge connects.",
        ToolAccess::Read,
    )
}

pub fn memory_resolve_metadata() -> ToolFunctionMetadata {
    build_memory_metadata::<MemoryResolveOpenInput, MemoryResolveSendInput, MemoryResolveNextOutput>(
        "memory/resolve",
        "Get the current truth for a node by following supersedes chains. \
         If a fact was corrected, this returns the latest version.",
        ToolAccess::Read,
    )
}

pub fn memory_impact_metadata() -> ToolFunctionMetadata {
    build_memory_metadata::<MemoryImpactOpenInput, MemoryImpactSendInput, MemoryImpactNextOutput>(
        "memory/impact",
        "Analyze causal dependencies: what decisions and inferences depend on a given node? \
         Use this to understand the blast radius before correcting a fact.",
        ToolAccess::Read,
    )
}

pub fn memory_link_metadata() -> ToolFunctionMetadata {
    build_memory_metadata::<MemoryLinkOpenInput, MemoryLinkSendInput, MemoryLinkNextOutput>(
        "memory/link",
        "Create typed edges between existing memory nodes. \
         Edge types: caused_by, supports, contradicts, supersedes, related_to, part_of, temporal_next.",
        ToolAccess::Write,
    )
}

pub fn memory_stats_metadata() -> ToolFunctionMetadata {
    build_memory_metadata::<MemoryStatsOpenInput, MemoryStatsSendInput, MemoryStatsNextOutput>(
        "memory/stats",
        "Get a health report for the agent's memory graph: node/edge counts, \
         contradictions, stale knowledge, orphan nodes, unsupported decisions.",
        ToolAccess::Read,
    )
}

register_tool!(memory_add_metadata, memory_tool_build_unused);
register_tool!(memory_search_metadata, memory_tool_build_unused);
register_tool!(memory_traverse_metadata, memory_tool_build_unused);
register_tool!(memory_resolve_metadata, memory_tool_build_unused);
register_tool!(memory_impact_metadata, memory_tool_build_unused);
register_tool!(memory_link_metadata, memory_tool_build_unused);
register_tool!(memory_stats_metadata, memory_tool_build_unused);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_tools_set_access_levels() {
        assert_eq!(memory_add_metadata().access, Some(ToolAccess::Write));
        assert_eq!(memory_link_metadata().access, Some(ToolAccess::Write));
        assert_eq!(memory_search_metadata().access, Some(ToolAccess::Read));
        assert_eq!(memory_traverse_metadata().access, Some(ToolAccess::Read));
        assert_eq!(memory_resolve_metadata().access, Some(ToolAccess::Read));
        assert_eq!(memory_impact_metadata().access, Some(ToolAccess::Read));
        assert_eq!(memory_stats_metadata().access, Some(ToolAccess::Read));
    }
}
