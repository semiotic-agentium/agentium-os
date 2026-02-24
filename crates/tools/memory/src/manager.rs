//! MemoryManager — owns the graph, engines, and file path for one agent's memory.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};

use agentic_memory::{
    AmemReader, AmemWriter, CausalParams, CognitiveEventBuilder, DEFAULT_DIMENSION, Edge, EdgeType,
    EventType, MAX_EDGES_PER_NODE, MemoryGraph, MemoryQualityParams, QueryEngine, TextSearchParams,
    TraversalDirection, TraversalParams, WriteEngine,
};
use baml_rt_core::AgentPackageName;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::types::*;

/// Errors specific to the memory manager.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(
        "invalid agent name for memory file path: {0} (expected ASCII [a-zA-Z0-9_-], no whitespace)"
    )]
    InvalidAgentName(String),
    #[error(
        "unknown event type: {0} (expected: fact, decision, inference, correction, skill, episode)"
    )]
    UnknownEventType(String),
    #[error(
        "unknown edge type: {0} (expected: caused_by, supports, contradicts, supersedes, related_to, part_of, temporal_next)"
    )]
    UnknownEdgeType(String),
    #[error("unknown direction: {0} (expected: forward, backward, both)")]
    UnknownDirection(String),
    #[error("agentic-memory error: {0}")]
    Amem(#[from] agentic_memory::AmemError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("memory file is already locked by another process: {0}")]
    LockBusy(String),
    #[error("memory manager registry mutex poisoned")]
    RegistryPoisoned,
    #[error(
        "agentic-memory assigned unexpected node IDs during ingest (expected {expected:?}, got {actual:?})"
    )]
    UnexpectedIngestNodeIds {
        expected: Vec<u64>,
        actual: Vec<u64>,
    },
}

#[derive(Debug)]
struct MemoryFileLock {
    // Held to keep the OS file lock alive for the manager lifetime; Drop releases the lock.
    #[allow(dead_code)]
    file: File,
}

impl MemoryFileLock {
    fn acquire(memory_file_path: &Path) -> Result<Self> {
        let lock_path = path_with_appended_suffix(memory_file_path, ".lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        try_lock_exclusive(&file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                MemoryError::LockBusy(lock_path.display().to_string())
            } else {
                MemoryError::Io(e)
            }
        })?;
        Ok(Self { file })
    }
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> std::io::Result<()> {
    // Best-effort fallback for non-Unix builds. The memory file remains process-local protected
    // by the in-process RwLock, but cross-process writer exclusion is not enforced here.
    Ok(())
}

#[cfg(unix)]
impl Drop for MemoryFileLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

type Result<T> = std::result::Result<T, MemoryError>;

/// Manages a single agent's memory file and graph.
pub struct MemoryManager {
    graph: Arc<RwLock<MemoryGraph>>,
    file_path: PathBuf,
    // Held for RAII: dropping the manager releases the file lock via MemoryFileLock::Drop.
    #[allow(dead_code)]
    lock: MemoryFileLock,
    query_engine: QueryEngine,
    write_engine: WriteEngine,
}

fn shared_manager_registry() -> &'static StdMutex<HashMap<PathBuf, Weak<MemoryManager>>> {
    static REGISTRY: OnceLock<StdMutex<HashMap<PathBuf, Weak<MemoryManager>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| StdMutex::new(HashMap::new()))
}

impl MemoryManager {
    /// Open (or create) a memory file for the given agent.
    ///
    /// Files live at `~/.brain/{agent_name}.amem` by default, configurable via `BRAIN_DIR`.
    pub fn open(agent_name: &str) -> Result<Self> {
        let agent_name = AgentPackageName::parse(agent_name)
            .ok_or_else(|| MemoryError::InvalidAgentName(agent_name.to_string()))?;
        let file_path = memory_file_path_for_agent(agent_name.as_str())?;
        Self::open_at(file_path)
    }

    /// Open (or reuse) a shared in-process memory manager for the given agent.
    ///
    /// This preserves single-writer locking across processes while allowing same-agent
    /// reloads/duplicate boots within one runner process to reuse the existing manager.
    pub fn open_shared(agent_name: &str) -> Result<Arc<Self>> {
        let agent_name = AgentPackageName::parse(agent_name)
            .ok_or_else(|| MemoryError::InvalidAgentName(agent_name.to_string()))?;
        let file_path = memory_file_path_for_agent(agent_name.as_str())?;
        Self::open_shared_at(file_path)
    }

    /// Open a memory file at an explicit path (for testing or custom locations).
    pub fn open_at(file_path: PathBuf) -> Result<Self> {
        Self::open_at_impl(file_path)
    }

    /// Open (or reuse) a shared in-process memory manager for an explicit path.
    pub fn open_shared_at(file_path: PathBuf) -> Result<Arc<Self>> {
        let key = file_path.clone();
        let registry = shared_manager_registry();
        let mut guard = registry.lock().map_err(|_| MemoryError::RegistryPoisoned)?;
        if let Some(existing) = guard.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }

        // Prune stale entries opportunistically while we hold the registry lock.
        guard.retain(|_, weak| weak.strong_count() > 0);

        let manager = Arc::new(Self::open_at_impl(file_path)?);
        guard.insert(key, Arc::downgrade(&manager));
        Ok(manager)
    }

    fn open_at_impl(file_path: PathBuf) -> Result<Self> {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let lock = MemoryFileLock::acquire(&file_path)?;
        let graph = if file_path.exists() {
            info!(path = %file_path.display(), "loading existing memory file");
            AmemReader::read_from_file(&file_path)?
        } else {
            info!(path = %file_path.display(), "creating new memory file");
            MemoryGraph::new(DEFAULT_DIMENSION)
        };

        Ok(Self {
            graph: Arc::new(RwLock::new(graph)),
            file_path,
            lock,
            query_engine: QueryEngine::new(),
            write_engine: WriteEngine::new(DEFAULT_DIMENSION),
        })
    }

    /// Store cognitive events and edges.
    pub async fn add(&self, input: MemoryAddSendInput) -> Result<MemoryAddNextOutput> {
        let events = input
            .events
            .into_iter()
            .map(|e| {
                let event_type = parse_event_type(&e.event_type)?;
                let mut builder = CognitiveEventBuilder::new(event_type, &e.content);
                if let Some(sid) = e.session_id {
                    builder = builder.session_id(sid);
                }
                if let Some(conf) = e.confidence {
                    builder = builder.confidence(conf);
                }
                Ok(builder.build())
            })
            .collect::<Result<Vec<_>>>()?;

        let edges = match input.edges {
            Some(edge_inputs) => edge_inputs
                .into_iter()
                .map(|e| {
                    let edge_type = parse_edge_type(&e.edge_type)?;
                    Ok(Edge::new(
                        e.source,
                        e.target,
                        edge_type,
                        e.weight.unwrap_or(1.0),
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
            None => Vec::new(),
        };

        let mut graph = self.graph.write().await;
        validate_events_for_batch(&events, graph.dimension())?;
        let predicted_batch_node_ids = predicted_ingest_node_ids(&graph, events.len());
        let predicted_batch_id_set: HashSet<u64> =
            predicted_batch_node_ids.iter().copied().collect();
        validate_edges_for_batch(&graph, &edges, &predicted_batch_id_set)?;
        let snapshot = clone_graph_snapshot(&graph)?;
        let result = match self.write_engine.ingest(&mut graph, events, edges) {
            Ok(result) => result,
            Err(err) => {
                *graph = snapshot;
                return Err(err.into());
            }
        };
        if result.new_node_ids != predicted_batch_node_ids {
            let actual = result.new_node_ids.clone();
            *graph = snapshot;
            return Err(MemoryError::UnexpectedIngestNodeIds {
                expected: predicted_batch_node_ids,
                actual,
            });
        }
        if let Err(err) = self.persist(&graph) {
            *graph = snapshot;
            return Err(err);
        }

        Ok(MemoryAddNextOutput {
            node_ids: result.new_node_ids,
            edge_count: result.new_edge_count,
            done: true,
        })
    }

    /// BM25 text search.
    pub async fn search(&self, input: MemorySearchSendInput) -> Result<MemorySearchNextOutput> {
        let event_types = parse_event_types_opt(input.types.as_deref())?;
        let session_ids = input.sessions.unwrap_or_default();
        let max_results = input.max.unwrap_or(10);

        let graph = self.graph.read().await;
        let params = TextSearchParams {
            query: input.query,
            max_results,
            event_types,
            session_ids,
            min_score: 0.0,
        };
        let results = self.query_engine.text_search(
            &graph,
            graph.term_index.as_ref(),
            graph.doc_lengths.as_ref(),
            params,
        )?;

        let matches = results
            .into_iter()
            .filter_map(|m| {
                graph.get_node(m.node_id).map(|node| MemorySearchMatch {
                    id: node.id,
                    score: m.score,
                    content: node.content.clone(),
                    event_type: node.event_type.name().to_string(),
                    session_id: (node.session_id != 0).then_some(node.session_id),
                    confidence: node.confidence,
                })
            })
            .collect();

        Ok(MemorySearchNextOutput {
            matches,
            done: true,
        })
    }

    /// Graph traversal from a starting node.
    pub async fn traverse(
        &self,
        input: MemoryTraverseSendInput,
    ) -> Result<MemoryTraverseNextOutput> {
        let edge_types = parse_edge_types_opt(input.edge_types.as_deref())?;
        let direction = input
            .direction
            .as_deref()
            .map(parse_direction)
            .transpose()?
            .unwrap_or(TraversalDirection::Forward);
        let max_depth = input.depth.unwrap_or(3);

        let graph = self.graph.read().await;
        let params = TraversalParams {
            start_id: input.start_id,
            edge_types,
            direction,
            max_depth,
            max_results: 100,
            min_confidence: 0.0,
        };
        let result = self.query_engine.traverse(&graph, params)?;

        let nodes = result
            .visited
            .iter()
            .filter_map(|&id| {
                graph.get_node(id).map(|node| TraversalNode {
                    id: node.id,
                    content: node.content.clone(),
                    event_type: node.event_type.name().to_string(),
                    confidence: node.confidence,
                    depth: result.depths.get(&id).copied().unwrap_or(0),
                })
            })
            .collect();

        let edges = result
            .edges_traversed
            .iter()
            .map(|e| TraversalEdge {
                source: e.source_id,
                target: e.target_id,
                edge_type: e.edge_type.name().to_string(),
                weight: e.weight,
            })
            .collect();

        Ok(MemoryTraverseNextOutput {
            nodes,
            edges,
            done: true,
        })
    }

    /// Follow supersedes chain to get current truth.
    pub async fn resolve(&self, input: MemoryResolveSendInput) -> Result<MemoryResolveNextOutput> {
        let graph = self.graph.read().await;
        let resolved = self.query_engine.resolve(&graph, input.node_id)?;
        let was_superseded = resolved.id != input.node_id;

        Ok(MemoryResolveNextOutput {
            id: resolved.id,
            content: resolved.content.clone(),
            event_type: resolved.event_type.name().to_string(),
            confidence: resolved.confidence,
            was_superseded,
            done: true,
        })
    }

    /// Causal dependency analysis.
    pub async fn impact(&self, input: MemoryImpactSendInput) -> Result<MemoryImpactNextOutput> {
        let graph = self.graph.read().await;
        let params = CausalParams {
            node_id: input.node_id,
            max_depth: input.depth.unwrap_or(5),
            dependency_types: vec![EdgeType::CausedBy, EdgeType::Supports, EdgeType::Supersedes],
        };
        let result = self.query_engine.causal(&graph, params)?;

        Ok(MemoryImpactNextOutput {
            dependent_count: result.dependents.len(),
            affected_decisions: result.affected_decisions,
            affected_inferences: result.affected_inferences,
            dependents: result.dependents,
            done: true,
        })
    }

    /// Create edges between existing nodes.
    pub async fn link(&self, input: MemoryLinkSendInput) -> Result<MemoryLinkNextOutput> {
        let edges = input
            .edges
            .into_iter()
            .map(|e| {
                let edge_type = parse_edge_type(&e.edge_type)?;
                Ok(Edge::new(
                    e.source,
                    e.target,
                    edge_type,
                    e.weight.unwrap_or(1.0),
                ))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut graph = self.graph.write().await;
        validate_edges_for_batch(&graph, &edges, &HashSet::new())?;
        let snapshot = clone_graph_snapshot(&graph)?;
        let mut count = 0;
        for edge in edges {
            if let Err(err) = graph.add_edge(edge) {
                *graph = snapshot;
                return Err(err.into());
            }
            count += 1;
        }
        graph.ensure_adjacency();
        if let Err(err) = self.persist(&graph) {
            *graph = snapshot;
            return Err(err);
        }

        Ok(MemoryLinkNextOutput {
            edges_created: count,
            done: true,
        })
    }

    /// Memory quality report.
    pub async fn stats(&self) -> Result<MemoryStatsNextOutput> {
        let graph = self.graph.read().await;
        let report = self
            .query_engine
            .memory_quality(&graph, MemoryQualityParams::default())?;

        Ok(MemoryStatsNextOutput {
            status: report.status,
            node_count: report.node_count,
            edge_count: report.edge_count,
            contradiction_edges: report.contradiction_edges,
            supersedes_edges: report.supersedes_edges,
            low_confidence_count: report.low_confidence_count,
            stale_count: report.stale_count,
            orphan_count: report.orphan_count,
            unsupported_decisions: report.decisions_without_support_count,
            file_path: self.file_path.display().to_string(),
            done: true,
        })
    }

    /// Persist the graph to disk via temp-file replace (Unix atomic rename; Windows fallback).
    fn persist(&self, graph: &MemoryGraph) -> Result<()> {
        let writer = AmemWriter::new(graph.dimension());
        let tmp_path = path_with_appended_suffix(&self.file_path, ".tmp");
        writer.write_to_file(graph, &tmp_path)?;
        replace_file_portable(&tmp_path, &self.file_path)?;
        debug!(path = %self.file_path.display(), "memory persisted");
        Ok(())
    }

    /// Get the file path for this agent's memory.
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }
}

fn memory_file_path_for_agent(agent_name: &str) -> Result<PathBuf> {
    let brain_dir = std::env::var("BRAIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_or_home().join(".brain"));
    std::fs::create_dir_all(&brain_dir)?;
    Ok(brain_dir.join(format!("{agent_name}.amem")))
}

fn path_with_appended_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut out = path.to_path_buf();
    let mut file_name = out
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| out.as_os_str().to_os_string());
    file_name.push(suffix);
    out.set_file_name(file_name);
    out
}

fn replace_file_portable(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        match std::fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(err) if dst.exists() => {
                std::fs::remove_file(dst)?;
                std::fs::rename(src, dst).map_err(|rename_err| {
                    // Prefer the retry failure since that is the post-fallback result.
                    let _ = err;
                    rename_err
                })
            }
            Err(err) => Err(err),
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(src, dst)
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_event_type(s: &str) -> Result<EventType> {
    EventType::from_name(s).ok_or_else(|| MemoryError::UnknownEventType(s.to_string()))
}

fn parse_edge_type(s: &str) -> Result<EdgeType> {
    EdgeType::from_name(s).ok_or_else(|| MemoryError::UnknownEdgeType(s.to_string()))
}

fn parse_direction(s: &str) -> Result<TraversalDirection> {
    match s.to_lowercase().as_str() {
        "forward" => Ok(TraversalDirection::Forward),
        "backward" => Ok(TraversalDirection::Backward),
        "both" => Ok(TraversalDirection::Both),
        _ => Err(MemoryError::UnknownDirection(s.to_string())),
    }
}

fn parse_event_types_opt(types: Option<&[String]>) -> Result<Vec<EventType>> {
    match types {
        Some(ts) => ts.iter().map(|s| parse_event_type(s)).collect(),
        None => Ok(Vec::new()),
    }
}

fn parse_edge_types_opt(types: Option<&[String]>) -> Result<Vec<EdgeType>> {
    match types {
        Some(ts) => ts.iter().map(|s| parse_edge_type(s)).collect(),
        None => Ok(vec![
            EdgeType::CausedBy,
            EdgeType::Supports,
            EdgeType::Contradicts,
            EdgeType::Supersedes,
            EdgeType::RelatedTo,
            EdgeType::PartOf,
            EdgeType::TemporalNext,
        ]),
    }
}

fn validate_events_for_batch(
    events: &[agentic_memory::CognitiveEvent],
    dimension: usize,
) -> Result<()> {
    for event in events {
        event.validate(dimension)?;
    }
    Ok(())
}

fn validate_edges_for_batch(
    graph: &MemoryGraph,
    edges: &[Edge],
    additional_valid_node_ids: &HashSet<u64>,
) -> Result<()> {
    let existing_node_ids: HashSet<u64> = graph.nodes().iter().map(|n| n.id).collect();
    let mut source_edge_counts: HashMap<u64, usize> = HashMap::new();
    for edge in graph.edges() {
        *source_edge_counts.entry(edge.source_id).or_insert(0) += 1;
    }

    for edge in edges {
        if edge.source_id == edge.target_id {
            return Err(agentic_memory::AmemError::SelfEdge(edge.source_id).into());
        }
        let source_exists = existing_node_ids.contains(&edge.source_id)
            || additional_valid_node_ids.contains(&edge.source_id);
        if !source_exists {
            return Err(agentic_memory::AmemError::NodeNotFound(edge.source_id).into());
        }
        let target_exists = existing_node_ids.contains(&edge.target_id)
            || additional_valid_node_ids.contains(&edge.target_id);
        if !target_exists {
            return Err(agentic_memory::AmemError::InvalidEdgeTarget(edge.target_id).into());
        }

        let count = source_edge_counts.entry(edge.source_id).or_insert(0);
        if *count >= usize::from(MAX_EDGES_PER_NODE) {
            return Err(agentic_memory::AmemError::TooManyEdges(MAX_EDGES_PER_NODE).into());
        }
        *count += 1;
    }

    Ok(())
}

fn predicted_ingest_node_ids(graph: &MemoryGraph, event_count: usize) -> Vec<u64> {
    if event_count == 0 {
        return Vec::new();
    }

    // `agentic-memory` currently assigns monotonically increasing IDs. Our memory tools do not
    // expose deletion, so `max(existing_id) + 1` matches the next assigned ID for tool-managed
    // files. `add()` verifies the returned IDs and rolls back if this assumption stops holding.
    let next_id = graph
        .nodes()
        .iter()
        .map(|n| n.id)
        .max()
        .map(|id| id + 1)
        .unwrap_or(0);
    (0..event_count)
        .map(|offset| next_id + offset as u64)
        .collect()
}

fn clone_graph_snapshot(graph: &MemoryGraph) -> Result<MemoryGraph> {
    let writer = AmemWriter::new(graph.dimension());
    let mut buf = Vec::new();
    writer.write_to(graph, &mut buf)?;
    let mut cursor = std::io::Cursor::new(buf);
    Ok(AmemReader::read_from(&mut cursor)?)
}

/// Home directory fallback.
fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager(name: &str) -> (tempfile::TempDir, MemoryManager) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(format!("{name}.amem"));
        let mgr = MemoryManager::open_at(path).unwrap();
        (tmp, mgr)
    }

    #[tokio::test]
    async fn test_add_search_cycle() {
        let (_tmp, mgr) = test_manager("test-agent");

        let add_result = mgr
            .add(MemoryAddSendInput {
                events: vec![MemoryEventInput {
                    event_type: "fact".to_string(),
                    content: "The sky is blue on clear days".to_string(),
                    session_id: Some(1),
                    confidence: Some(0.95),
                }],
                edges: None,
            })
            .await
            .unwrap();

        assert_eq!(add_result.node_ids.len(), 1);
        assert!(add_result.done);

        let search_result = mgr
            .search(MemorySearchSendInput {
                query: "sky blue".to_string(),
                types: None,
                sessions: None,
                max: None,
            })
            .await
            .unwrap();

        assert!(!search_result.matches.is_empty());
        assert_eq!(search_result.matches[0].event_type, "fact");
        assert!(search_result.done);

        let stats = mgr.stats().await.unwrap();
        assert_eq!(stats.node_count, 1);
        assert!(stats.done);

        assert!(mgr.file_path().exists());
    }

    #[tokio::test]
    async fn test_traverse_and_resolve() {
        let (_tmp, mgr) = test_manager("test-traverse");

        let result = mgr
            .add(MemoryAddSendInput {
                events: vec![
                    MemoryEventInput {
                        event_type: "fact".to_string(),
                        content: "Users prefer dark mode".to_string(),
                        session_id: Some(1),
                        confidence: Some(0.8),
                    },
                    MemoryEventInput {
                        event_type: "decision".to_string(),
                        content: "Default to dark mode theme".to_string(),
                        session_id: Some(1),
                        confidence: Some(0.9),
                    },
                ],
                edges: None,
            })
            .await
            .unwrap();

        let fact_id = result.node_ids[0];
        let decision_id = result.node_ids[1];

        mgr.link(MemoryLinkSendInput {
            edges: vec![MemoryEdgeInput {
                source: decision_id,
                target: fact_id,
                edge_type: "caused_by".to_string(),
                weight: Some(1.0),
            }],
        })
        .await
        .unwrap();

        let traversal = mgr
            .traverse(MemoryTraverseSendInput {
                start_id: decision_id,
                edge_types: None,
                direction: Some("forward".to_string()),
                depth: Some(2),
            })
            .await
            .unwrap();

        assert!(!traversal.nodes.is_empty());
        assert!(traversal.done);

        let resolved = mgr
            .resolve(MemoryResolveSendInput { node_id: fact_id })
            .await
            .unwrap();

        assert!(!resolved.was_superseded);
        assert_eq!(resolved.id, fact_id);
    }

    #[tokio::test]
    async fn test_add_is_atomic_on_invalid_edge() {
        let (_tmp, mgr) = test_manager("test-atomic-add");

        let err = mgr
            .add(MemoryAddSendInput {
                events: vec![MemoryEventInput {
                    event_type: "fact".to_string(),
                    content: "Transient fact".to_string(),
                    session_id: Some(1),
                    confidence: Some(0.9),
                }],
                edges: Some(vec![MemoryEdgeInput {
                    source: 0,
                    target: 999_999,
                    edge_type: "supports".to_string(),
                    weight: Some(1.0),
                }]),
            })
            .await
            .expect_err("invalid edge target should fail");
        assert!(
            err.to_string().contains("invalid node")
                || err.to_string().contains("Node ID")
                || err.to_string().contains("Edge references invalid"),
            "unexpected error: {err}"
        );

        let stats = mgr.stats().await.expect("stats after failed add");
        assert_eq!(
            stats.node_count, 0,
            "failed add must not leave partial node"
        );
        assert_eq!(
            stats.edge_count, 0,
            "failed add must not leave partial edges"
        );
    }

    #[tokio::test]
    async fn test_add_allows_edges_between_new_nodes_in_same_batch() {
        let (_tmp, mgr) = test_manager("test-cobatch-edges");

        let add = mgr
            .add(MemoryAddSendInput {
                events: vec![
                    MemoryEventInput {
                        event_type: "fact".to_string(),
                        content: "A".to_string(),
                        session_id: None,
                        confidence: Some(0.8),
                    },
                    MemoryEventInput {
                        event_type: "decision".to_string(),
                        content: "B".to_string(),
                        session_id: None,
                        confidence: Some(0.9),
                    },
                ],
                // Empty graph predicts new IDs 0 and 1.
                edges: Some(vec![MemoryEdgeInput {
                    source: 1,
                    target: 0,
                    edge_type: "caused_by".to_string(),
                    weight: Some(1.0),
                }]),
            })
            .await
            .expect("co-batch edge should validate and persist");

        assert_eq!(add.node_ids, vec![0, 1]);
        assert_eq!(add.edge_count, 1);
    }

    #[tokio::test]
    async fn test_link_is_atomic_on_invalid_edge() {
        let (_tmp, mgr) = test_manager("test-atomic-link");

        let add = mgr
            .add(MemoryAddSendInput {
                events: vec![
                    MemoryEventInput {
                        event_type: "fact".to_string(),
                        content: "A".to_string(),
                        session_id: Some(1),
                        confidence: Some(1.0),
                    },
                    MemoryEventInput {
                        event_type: "fact".to_string(),
                        content: "B".to_string(),
                        session_id: Some(1),
                        confidence: Some(1.0),
                    },
                ],
                edges: None,
            })
            .await
            .expect("seed memory");
        let a = add.node_ids[0];
        let b = add.node_ids[1];

        let err = mgr
            .link(MemoryLinkSendInput {
                edges: vec![
                    MemoryEdgeInput {
                        source: a,
                        target: b,
                        edge_type: "supports".to_string(),
                        weight: Some(1.0),
                    },
                    MemoryEdgeInput {
                        source: a,
                        target: 999_999,
                        edge_type: "supports".to_string(),
                        weight: Some(1.0),
                    },
                ],
            })
            .await
            .expect_err("invalid edge target should fail");
        assert!(
            err.to_string().contains("invalid node")
                || err.to_string().contains("Edge references invalid"),
            "unexpected error: {err}"
        );

        let stats = mgr.stats().await.expect("stats after failed link");
        assert_eq!(
            stats.edge_count, 0,
            "failed link must not leave partial edges"
        );
    }

    #[tokio::test]
    async fn test_search_omits_session_id_when_unset() {
        let (_tmp, mgr) = test_manager("test-search-session-none");

        mgr.add(MemoryAddSendInput {
            events: vec![MemoryEventInput {
                event_type: "fact".to_string(),
                content: "Unscoped memory".to_string(),
                session_id: None,
                confidence: Some(1.0),
            }],
            edges: None,
        })
        .await
        .expect("seed memory");

        let result = mgr
            .search(MemorySearchSendInput {
                query: "Unscoped".to_string(),
                types: None,
                sessions: None,
                max: Some(1),
            })
            .await
            .expect("search");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].session_id, None);
    }

    #[test]
    fn test_open_rejects_invalid_agent_name() {
        let err = match MemoryManager::open("../escape") {
            Ok(_) => panic!("invalid name should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("invalid agent name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_open_shared_reuses_same_manager_for_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shared-reload.amem");

        let first = MemoryManager::open_shared_at(path.clone()).expect("first shared open");
        let second = MemoryManager::open_shared_at(path).expect("second shared open");

        assert!(
            Arc::ptr_eq(&first, &second),
            "same-path shared opens must reuse the in-process manager"
        );
    }

    #[test]
    fn test_appended_suffix_paths_preserve_custom_extensions() {
        let db = PathBuf::from("/tmp/foo.db");
        let json = PathBuf::from("/tmp/foo.json");

        let db_lock = path_with_appended_suffix(&db, ".lock");
        let json_lock = path_with_appended_suffix(&json, ".lock");
        let db_tmp = path_with_appended_suffix(&db, ".tmp");
        let json_tmp = path_with_appended_suffix(&json, ".tmp");

        assert_ne!(db_lock, json_lock, "lock paths must not collide by stem");
        assert_ne!(db_tmp, json_tmp, "temp paths must not collide by stem");
        assert!(db_lock.ends_with("foo.db.lock"));
        assert!(db_tmp.ends_with("foo.db.tmp"));
    }
}
