//! Tool metadata registration for the memory bundle.

use baml_rt_tools::baml_tool;

use crate::types::*;

#[baml_tool(
    name = "memory/add",
    description = "Store cognitive events (facts, decisions, inferences, corrections, skills, episodes) \
        with optional edges. Returns the IDs of newly created nodes.",
    tags = ["memory"],
    access = Write,
    metadata_only,
    open_input = MemoryAddOpenInput,
    input = MemoryAddSendInput,
    output = MemoryAddNextOutput,
)]
pub struct MemoryAdd;

#[baml_tool(
    name = "memory/search",
    description = "BM25 text search over stored memories. Returns ranked matches with content and metadata. \
        Use this to recall relevant knowledge before making decisions.",
    tags = ["memory"],
    access = Read,
    metadata_only,
    open_input = MemorySearchOpenInput,
    input = MemorySearchSendInput,
    output = MemorySearchNextOutput,
)]
pub struct MemorySearch;

#[baml_tool(
    name = "memory/traverse",
    description = "Walk reasoning chains in the memory graph from a starting node. \
        Follow typed edges (caused_by, supports, contradicts, etc.) to explore how knowledge connects.",
    tags = ["memory"],
    access = Read,
    metadata_only,
    open_input = MemoryTraverseOpenInput,
    input = MemoryTraverseSendInput,
    output = MemoryTraverseNextOutput,
)]
pub struct MemoryTraverse;

#[baml_tool(
    name = "memory/resolve",
    description = "Get the current truth for a node by following supersedes chains. \
        If a fact was corrected, this returns the latest version.",
    tags = ["memory"],
    access = Read,
    metadata_only,
    open_input = MemoryResolveOpenInput,
    input = MemoryResolveSendInput,
    output = MemoryResolveNextOutput,
)]
pub struct MemoryResolve;

#[baml_tool(
    name = "memory/impact",
    description = "Analyze causal dependencies: what decisions and inferences depend on a given node? \
        Use this to understand the blast radius before correcting a fact.",
    tags = ["memory"],
    access = Read,
    metadata_only,
    open_input = MemoryImpactOpenInput,
    input = MemoryImpactSendInput,
    output = MemoryImpactNextOutput,
)]
pub struct MemoryImpact;

#[baml_tool(
    name = "memory/link",
    description = "Create typed edges between existing memory nodes. \
        Edge types: caused_by, supports, contradicts, supersedes, related_to, part_of, temporal_next.",
    tags = ["memory"],
    access = Write,
    metadata_only,
    open_input = MemoryLinkOpenInput,
    input = MemoryLinkSendInput,
    output = MemoryLinkNextOutput,
)]
pub struct MemoryLink;

#[baml_tool(
    name = "memory/stats",
    description = "Get a health report for the agent's memory graph: node/edge counts, \
        contradictions, stale knowledge, orphan nodes, unsupported decisions.",
    tags = ["memory"],
    access = Read,
    metadata_only,
    open_input = MemoryStatsOpenInput,
    input = MemoryStatsSendInput,
    output = MemoryStatsNextOutput,
)]
pub struct MemoryStats;

#[cfg(test)]
mod tests {
    use super::*;
    use baml_rt_tools::tools::ToolAccess;

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
