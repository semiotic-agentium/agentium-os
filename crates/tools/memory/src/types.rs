//! Tool-facing types for the memory bundle.
//!
//! Enum-like fields use explicit tool enums (not raw strings) so the generated
//! schema/TypeScript bindings are self-documenting and invalid values are
//! rejected at deserialization time.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// memory/add
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEventType {
    Fact,
    Decision,
    Inference,
    Correction,
    Skill,
    Episode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEdgeType {
    CausedBy,
    Supports,
    Contradicts,
    Supersedes,
    RelatedTo,
    PartOf,
    TemporalNext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTraversalDirection {
    Forward,
    Backward,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MemoryHealthStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEventInput {
    /// Event type.
    pub event_type: MemoryEventType,
    /// The content of this cognitive event.
    pub content: String,
    /// Session identifier (groups related events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u32>,
    /// Confidence level (0.0 to 1.0, default 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEdgeInput {
    /// Source node ID.
    pub source: u64,
    /// Target node ID.
    pub target: u64,
    /// Edge type.
    pub edge_type: MemoryEdgeType,
    /// Edge weight (0.0 to 1.0, default 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAddOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAddSendInput {
    /// Cognitive events to store.
    pub events: Vec<MemoryEventInput>,
    /// Edges to create. May reference existing nodes and (for advanced callers) predicted IDs
    /// of nodes created in this same batch.
    /// Prefer `memory/link` after `memory/add` returns node IDs when you do not know IDs upfront.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges: Option<Vec<MemoryEdgeInput>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAddNextOutput {
    /// IDs of newly created nodes (in the same order as input events).
    pub node_ids: Vec<u64>,
    /// Number of edges created.
    pub edge_count: usize,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// memory/search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchSendInput {
    /// Text query (BM25 ranked).
    pub query: String,
    /// Filter by event types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<MemoryEventType>>,
    /// Filter by session IDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<u32>>,
    /// Maximum number of results (default 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchMatch {
    pub id: u64,
    pub score: f32,
    pub content: String,
    pub event_type: MemoryEventType,
    /// Session ID when present; omitted if the node was written without session attribution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u32>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchNextOutput {
    pub matches: Vec<MemorySearchMatch>,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// memory/traverse
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTraverseOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTraverseSendInput {
    /// Starting node ID.
    pub start_id: u64,
    /// Edge types to follow (default: all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<MemoryEdgeType>>,
    /// Traversal direction (default: `forward`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<MemoryTraversalDirection>,
    /// Maximum traversal depth (default: 3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TraversalNode {
    pub id: u64,
    pub content: String,
    pub event_type: MemoryEventType,
    pub confidence: f32,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TraversalEdge {
    pub source: u64,
    pub target: u64,
    pub edge_type: MemoryEdgeType,
    pub weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTraverseNextOutput {
    pub nodes: Vec<TraversalNode>,
    pub edges: Vec<TraversalEdge>,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// memory/resolve
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryResolveOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryResolveSendInput {
    /// Node ID to resolve (follows supersedes chain to current truth).
    pub node_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryResolveNextOutput {
    pub id: u64,
    pub content: String,
    pub event_type: MemoryEventType,
    pub confidence: f32,
    /// True if the original node was superseded (resolved to a different node).
    pub was_superseded: bool,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// memory/impact
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryImpactOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryImpactSendInput {
    /// Node ID to analyze impact for.
    pub node_id: u64,
    /// Maximum traversal depth (default: 5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryImpactNextOutput {
    pub dependent_count: usize,
    pub affected_decisions: usize,
    pub affected_inferences: usize,
    pub dependents: Vec<u64>,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// memory/link
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryLinkOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryLinkSendInput {
    /// Edges to create.
    pub edges: Vec<MemoryEdgeInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryLinkNextOutput {
    pub edges_created: usize,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// memory/stats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatsOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatsSendInput {
    /// Reserved optional field: the BAML schema generator currently rejects
    /// empty classes, so this keeps the wire input effectively `{}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub reserved: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatsNextOutput {
    /// Overall health.
    pub status: MemoryHealthStatus,
    pub node_count: usize,
    pub edge_count: usize,
    pub contradiction_edges: usize,
    pub supersedes_edges: usize,
    pub low_confidence_count: usize,
    pub stale_count: usize,
    pub orphan_count: usize,
    pub unsupported_decisions: usize,
    pub file_path: String,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// DescribeAction impls
// ---------------------------------------------------------------------------

use baml_rt_tools::DescribeAction;

impl DescribeAction for MemoryAddOpenInput {
    fn describe(&self) -> String {
        "storing memories".to_string()
    }
}
impl DescribeAction for MemoryAddSendInput {
    fn describe(&self) -> String {
        format!("storing {} memory event(s)", self.events.len())
    }
}
impl DescribeAction for MemorySearchOpenInput {
    fn describe(&self) -> String {
        "searching memory".to_string()
    }
}
impl DescribeAction for MemorySearchSendInput {
    fn describe(&self) -> String {
        format!("searching memory for '{}'", self.query)
    }
}
impl DescribeAction for MemoryTraverseOpenInput {
    fn describe(&self) -> String {
        "traversing memory graph".to_string()
    }
}
impl DescribeAction for MemoryTraverseSendInput {
    fn describe(&self) -> String {
        "traversing memory graph connections".to_string()
    }
}
impl DescribeAction for MemoryResolveOpenInput {
    fn describe(&self) -> String {
        "resolving memory node".to_string()
    }
}
impl DescribeAction for MemoryResolveSendInput {
    fn describe(&self) -> String {
        "resolving memory node to current truth".to_string()
    }
}
impl DescribeAction for MemoryImpactOpenInput {
    fn describe(&self) -> String {
        "analyzing memory impact".to_string()
    }
}
impl DescribeAction for MemoryImpactSendInput {
    fn describe(&self) -> String {
        "analyzing downstream impact of memory node".to_string()
    }
}
impl DescribeAction for MemoryLinkOpenInput {
    fn describe(&self) -> String {
        "linking memory nodes".to_string()
    }
}
impl DescribeAction for MemoryLinkSendInput {
    fn describe(&self) -> String {
        format!("creating {} memory edge(s)", self.edges.len())
    }
}
impl DescribeAction for MemoryStatsOpenInput {
    fn describe(&self) -> String {
        "retrieving memory statistics".to_string()
    }
}
impl DescribeAction for MemoryStatsSendInput {
    fn describe(&self) -> String {
        "retrieving memory graph statistics".to_string()
    }
}
