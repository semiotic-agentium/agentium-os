//! Mermaid sequence diagram renderer for [`ExportedGraph`].
//!
//! Produces a Mermaid `sequenceDiagram` that shows the temporal narrative of a
//! provenance graph: user prompts → LLM reasoning → tool calls → responses,
//! ordered chronologically with autonumbering.
//!
//! Unlike the flowchart renderer (`mermaid.rs`) which shows structural
//! relationships, this renderer is inherently temporal: participants line up
//! across the top and messages flow downward in causal order.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
};

use baml_rt_observability::record_provenance_sequence_render;

use super::{ExportedEdge, ExportedGraph, ExportedNode, activity_outcome::NodeActivityOutcome};
use crate::{
    graph_model::{
        EDGE_WAS_EMITTED_BY, EDGE_WAS_EXECUTED_BY, EDGE_WAS_GENERATED_BY, EDGE_WAS_INVOKED_BY,
        EDGE_WAS_RECEIVED_BY, EDGE_WAS_SPAWNED_BY, EDGE_WAS_UPDATED_BY, GraphNodeLabel,
    },
    spans,
    vocabulary::{a2a, agent_types, semantic_labels},
};

/// Maximum character length for content previews on sequence diagram arrows.
const SEQUENCE_CONTENT_PREVIEW_LEN: usize = 50;

/// Maximum character length for tool-args summaries on sequence diagram arrows.
const SEQUENCE_ARGS_SUMMARY_LEN: usize = 40;

/// Maximum character length for error message previews on failure arrows.
const SEQUENCE_ERROR_PREVIEW_LEN: usize = 80;

/// Mermaid arrow for failures: `--x` (dotted line with cross). Indicates a lost/destroyed
/// or failed message. Used for LLM and tool call failures; label includes ✗ and error preview.
const MERMAID_FAILURE_ARROW: &str = "--x";

/// Indices over an [`ExportedGraph`] for O(1) node lookup and O(degree) edge lookup.
struct GraphIndices<'a> {
    nodes_by_id: HashMap<&'a str, &'a ExportedNode>,
    edges_by_from: HashMap<&'a str, Vec<&'a ExportedEdge>>,
    edges_by_to: HashMap<&'a str, Vec<&'a ExportedEdge>>,
    /// Delegation edges only (WAS_DELEGATED_TO). Avoids O(E) scan per message.
    delegation_edges: Vec<&'a ExportedEdge>,
    /// Pre-parsed label per node id. Avoids repeated GraphNodeLabel::parse.
    label_by_node: HashMap<&'a str, Option<GraphNodeLabel>>,
}

impl<'a> GraphIndices<'a> {
    fn build(graph: &'a ExportedGraph) -> Self {
        let n = graph.nodes.len();
        let e = graph.edges.len();
        let mut nodes_by_id = HashMap::with_capacity(n);
        let mut label_by_node = HashMap::with_capacity(n);
        for node in &graph.nodes {
            let id = node.id.as_str();
            nodes_by_id.insert(id, node);
            label_by_node.insert(id, GraphNodeLabel::parse(&node.label));
        }
        let mut edges_by_from: HashMap<&str, Vec<&ExportedEdge>> = HashMap::with_capacity(e.min(n));
        let mut edges_by_to: HashMap<&str, Vec<&ExportedEdge>> = HashMap::with_capacity(e.min(n));
        let mut delegation_edges = Vec::new();
        for edge in &graph.edges {
            edges_by_from
                .entry(edge.from.as_str())
                .or_default()
                .push(edge);
            edges_by_to.entry(edge.to.as_str()).or_default().push(edge);
            if edge_relation_matches(&edge.relation, semantic_labels::WAS_DELEGATED_TO) {
                delegation_edges.push(edge);
            }
        }
        Self {
            nodes_by_id,
            edges_by_from,
            edges_by_to,
            delegation_edges,
            label_by_node,
        }
    }
}

/// Map: activity → AgentRuntimeInstance (by node id). archive_path is on the agent node.
struct ResolutionMaps<'a> {
    activity_to_agent: HashMap<&'a str, &'a str>,
}

impl<'a> ResolutionMaps<'a> {
    fn build(graph: &'a ExportedGraph, indices: &GraphIndices<'a>) -> Self {
        let mut activity_to_agent = HashMap::new();
        let is_agent = |id: &str| {
            indices.label_by_node.get(id).copied().flatten()
                == Some(GraphNodeLabel::AgentRuntimeInstance)
        };
        for edge in &graph.edges {
            if edge_relation_matches(&edge.relation, EDGE_WAS_EXECUTED_BY)
                && is_agent(edge.to.as_str())
            {
                activity_to_agent.insert(edge.from.as_str(), edge.to.as_str());
            }
        }
        for edge in &graph.edges {
            if edge_relation_matches(&edge.relation, EDGE_WAS_INVOKED_BY)
                && is_agent(edge.to.as_str())
            {
                activity_to_agent
                    .entry(edge.from.as_str())
                    .or_insert(edge.to.as_str());
            }
        }
        Self { activity_to_agent }
    }
}

/// Render an [`ExportedGraph`] as a Mermaid `sequenceDiagram` string.
///
/// The graph should be simplified (via [`super::simplify::simplify_graph`]) and
/// temporally sorted (nodes ordered by `event_order`) before calling this.
pub fn render_sequence_diagram(graph: &ExportedGraph) -> String {
    let scope_str = match &graph.scope {
        super::ExportScope::Context(_) => "context",
        super::ExportScope::Task(_) => "task",
        super::ExportScope::Full => "full",
    };
    let span = spans::sequence_render(graph.nodes.len(), graph.edges.len(), scope_str);
    let _guard = span.enter();
    let start = std::time::Instant::now();

    let indices = GraphIndices::build(graph);
    let resolution = ResolutionMaps::build(graph, &indices);
    let mut activity_cache = HashMap::new();
    let agent_for_node =
        build_agent_for_node_map(graph, &indices, &resolution, &mut activity_cache);
    let task_status_by_id = build_task_status_map(graph, &indices);

    let mut out = String::with_capacity(graph.nodes.len() * 80 + graph.edges.len() * 40);
    let _ = writeln!(out, "sequenceDiagram");
    let _ = writeln!(out, "    autonumber");

    // ── 1. Identify participants ────────────────────────────────────────
    let participants = extract_participants(graph, &indices, &resolution, &mut activity_cache);

    if participants.has_user {
        let _ = writeln!(out, "    actor User");
    }
    for agent in &participants.agents {
        let _ = writeln!(out, "    participant {}", sanitize_participant(agent));
    }
    for llm in &participants.llms {
        let _ = writeln!(out, "    participant {}", sanitize_participant(llm));
    }
    for tool in &participants.tools {
        let _ = writeln!(out, "    participant {}", sanitize_participant(tool));
    }
    let _ = writeln!(out);

    // ── 2. Walk nodes in emission order (event_order, then type priority so Message before TaskState), emit ─
    // First user message in the context should appear before task-internal events (status, LLM) that
    // may have lower event_order due to protocol sequencing (task created before message processed).
    let first_user_msg_id = graph
        .nodes
        .iter()
        .filter(|n| {
            indices.label_by_node.get(n.id.as_str()).copied().flatten()
                == Some(GraphNodeLabel::Message)
                && normalize_role(
                    n.properties
                        .get(a2a::ROLE)
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                ) == "user"
        })
        .min_by_key(|n| (n.event_order.unwrap_or(u64::MAX), &n.id))
        .map(|n| n.id.as_str());

    let turn_by_node = build_turn_by_node(graph, &indices);

    let mut emission_order: Vec<&ExportedNode> = graph.nodes.iter().collect();
    emission_order.sort_unstable_by(|a, b| {
        cmp_emission_order_with_turn(
            a.event_order,
            &a.id,
            indices.label_by_node.get(a.id.as_str()).copied().flatten(),
            turn_by_node.get(a.id.as_str()).copied(),
            b.event_order,
            &b.id,
            indices.label_by_node.get(b.id.as_str()).copied().flatten(),
            turn_by_node.get(b.id.as_str()).copied(),
            first_user_msg_id,
        )
    });

    let task_rect_spans = build_task_rect_spans(
        graph,
        &emission_order,
        &agent_for_node,
        &indices,
        &participants,
    );

    let mut current_rect_task: Option<String> = None;
    let mut last_activated_agent: Option<String> = None;
    let mut saw_user_message = false;
    for node in &emission_order {
        let task_id = prop_str(node, a2a::TASK_ID)
            .or_else(|| resolve_task_id_for_sequence_node(node, &indices));
        let will_emit = agent_for_node.contains_key(&node.id);
        let emits_visible = node_emits_visible_content(node, &indices);
        let is_user_msg = is_user_message(node);
        // Open a new task section when: (a) switching task_id, or (b) new user message (new turn)
        // after we've already emitted one. (b) handles resumed tasks where the same task_id is
        // used for both turns.
        let should_open_rect = will_emit && emits_visible && {
            let task_switch = task_id
                .as_ref()
                .is_some_and(|tid| current_rect_task.as_deref() != Some(tid.as_str()));
            let new_user_turn = is_user_msg && saw_user_message;
            task_switch || new_user_turn
        };
        // When the initiating user message triggers a new section, emit it BEFORE the section note.
        if should_open_rect && is_user_msg {
            if let Some(agent) = agent_for_node.get(&node.id)
                && indices
                    .label_by_node
                    .get(node.id.as_str())
                    .copied()
                    .flatten()
                    != Some(GraphNodeLabel::TaskState)
            {
                emit_node(
                    &mut out,
                    node,
                    &indices,
                    &resolution,
                    &participants,
                    agent,
                    &mut last_activated_agent,
                );
            }
            saw_user_message = true;
        }
        if should_open_rect {
            let tid = task_id.as_ref().or(current_rect_task.as_ref()).cloned();
            let is_new_turn_same_task =
                is_user_msg && saw_user_message && current_rect_task.as_deref() == tid.as_deref();
            let (should_open, note_tid) = if let Some(ref tid) = tid {
                let open = current_rect_task.as_deref() != Some(tid.as_str())
                    || (is_user_msg && saw_user_message);
                (open, Some(tid.clone()))
            } else if is_user_msg && saw_user_message {
                (true, None)
            } else {
                (false, None)
            };
            if should_open {
                if let Some(ref t) = note_tid {
                    current_rect_task = Some(t.clone());
                }
                let raw_status = note_tid
                    .as_ref()
                    .and_then(|t| task_status_by_id.get(t.as_str()))
                    .map(String::as_str);
                let span = note_tid
                    .as_ref()
                    .and_then(|t| task_rect_spans.get(&Some(t.clone())).cloned())
                    .or_else(|| task_rect_spans.get(&None).cloned())
                    .or_else(|| task_rect_note_span(&participants));
                if let Some((first, last)) = span {
                    let label = note_tid
                        .as_ref()
                        .and_then(|_| {
                            let raw = raw_status?;
                            let humanized = humanize_task_status(raw);
                            // New turn, same task_id: previous rect's "Completed" is stale.
                            // Show "Running" so the rect reflects this turn's FSM, not the last.
                            Some(if is_new_turn_same_task && raw == "TASK_STATE_COMPLETED" {
                                "Running".to_string()
                            } else {
                                humanized
                            })
                        })
                        .or_else(|| note_tid.clone())
                        .unwrap_or_else(|| "Continued".to_string());
                    let _ = writeln!(
                        out,
                        "    Note over {first},{last}: {}",
                        escape_note_content(&label)
                    );
                }
            }
        }
        // Emit node unless we already emitted it (initiating user message above rect).
        let already_emitted = should_open_rect && is_user_msg;
        if !already_emitted
            && let Some(agent) = agent_for_node.get(&node.id)
            && indices
                .label_by_node
                .get(node.id.as_str())
                .copied()
                .flatten()
                != Some(GraphNodeLabel::TaskState)
        {
            emit_node(
                &mut out,
                node,
                &indices,
                &resolution,
                &participants,
                agent,
                &mut last_activated_agent,
            );
        }
        if is_user_msg && will_emit {
            saw_user_message = true;
        }
    }

    let duration = start.elapsed();
    record_provenance_sequence_render(scope_str, duration, graph.nodes.len());

    out
}

/// True if this node is a Message with role=user (inbound from user).
fn is_user_message(node: &ExportedNode) -> bool {
    normalize_role(
        node.properties
            .get(a2a::ROLE)
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    ) == "user"
}

/// True if this node produces visible sequence diagram output (Message, LlmCall, ToolCall,
/// Artifact, PromptRejected). False for TaskExecution, MessageProcessing, TaskState.
fn node_emits_visible_content(node: &ExportedNode, indices: &GraphIndices<'_>) -> bool {
    matches!(
        indices
            .label_by_node
            .get(node.id.as_str())
            .copied()
            .flatten(),
        Some(GraphNodeLabel::Message)
            | Some(GraphNodeLabel::LlmCall)
            | Some(GraphNodeLabel::ToolCall)
            | Some(GraphNodeLabel::Artifact)
            | Some(GraphNodeLabel::PromptRejected)
    )
}

/// Resolve task_id for rect switching. Uses node properties first; for Message nodes
/// without task_id, derives from the MessageProcessing that received or emitted them.
fn resolve_task_id_for_sequence_node(
    node: &ExportedNode,
    indices: &GraphIndices<'_>,
) -> Option<String> {
    if let Some(tid) = prop_str(node, a2a::TASK_ID) {
        return Some(tid);
    }
    if indices
        .label_by_node
        .get(node.id.as_str())
        .copied()
        .flatten()
        != Some(GraphNodeLabel::Message)
    {
        return None;
    }
    // WAS_RECEIVED_BY: MessageProcessing -> Message. edge.to == node.id, from = mp.
    for edge in indices
        .edges_by_to
        .get(node.id.as_str())
        .into_iter()
        .flatten()
    {
        if edge_relation_matches(&edge.relation, EDGE_WAS_RECEIVED_BY) {
            let mp = indices.nodes_by_id.get(edge.from.as_str())?;
            if let Some(tid) = prop_str(mp, a2a::TASK_ID) {
                return Some(tid);
            }
        }
    }
    // WAS_EMITTED_BY: Message -> MessageProcessing. edge.from == node.id, to = mp.
    for edge in indices
        .edges_by_from
        .get(node.id.as_str())
        .into_iter()
        .flatten()
    {
        if edge_relation_matches(&edge.relation, EDGE_WAS_EMITTED_BY) {
            let mp = indices.nodes_by_id.get(edge.to.as_str())?;
            if let Some(tid) = prop_str(mp, a2a::TASK_ID) {
                return Some(tid);
            }
        }
    }
    None
}

/// Build task_id -> latest status from TaskState nodes (by event_order).
fn build_task_status_map(
    graph: &ExportedGraph,
    indices: &GraphIndices<'_>,
) -> HashMap<String, String> {
    let mut by_task: HashMap<String, (String, u64)> = HashMap::new();
    for node in &graph.nodes {
        if indices
            .label_by_node
            .get(node.id.as_str())
            .copied()
            .flatten()
            != Some(GraphNodeLabel::TaskState)
        {
            continue;
        }
        let task_id =
            prop_str(node, a2a::TASK_ID).or_else(|| parse_task_id_from_task_state_id(&node.id));
        let status = prop_str(node, a2a::TASK_STATE);
        let order = node.event_order.unwrap_or(0);
        if let (Some(tid), Some(st)) = (task_id, status) {
            let keep = by_task.get(&tid).is_none_or(|(_, o)| order > *o);
            if keep {
                by_task.insert(tid, (st, order));
            }
        }
    }
    by_task.into_iter().map(|(k, (v, _))| (k, v)).collect()
}

/// Parse task_id from TaskState node id: task_state:{task_id}:{status}.
fn parse_task_id_from_task_state_id(id: &str) -> Option<String> {
    id.strip_prefix("task_state:")
        .and_then(|rest| rest.split(':').next())
        .map(String::from)
}

/// Humanize status for display: strip TASK_STATE_ prefix, replace underscores with spaces, title-case.
fn humanize_task_status(raw: &str) -> String {
    let stripped = raw
        .strip_prefix("TASK_STATE_")
        .unwrap_or(raw)
        .to_lowercase();
    stripped
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().chain(c).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// First and last participant for a Note span across the diagram (task boundary label).
/// Uses global participants when task-scoped span is unavailable.
fn task_rect_note_span(participants: &Participants) -> Option<(String, String)> {
    let first = if participants.has_user {
        "User".to_string()
    } else {
        participants
            .agents
            .first()
            .map(|a| sanitize_participant(a))?
    };
    let last = participants
        .tools
        .last()
        .or(participants.llms.last())
        .or(participants.agents.last())
        .map(|s| sanitize_participant(s))
        .unwrap_or_else(|| first.clone());
    Some((first, last))
}

/// Task-scoped (first, last) for Note span. A task executes in the context of a SINGLE agent;
/// the rect spans only that agent and its tools/LLMs, never multiple agents.
fn build_task_rect_spans(
    _graph: &ExportedGraph,
    emission_order: &[&ExportedNode],
    agent_for_node: &HashMap<String, String>,
    indices: &GraphIndices<'_>,
    participants: &Participants,
) -> HashMap<Option<String>, (String, String)> {
    type TaskAccum = (Vec<String>, Vec<String>, Vec<String>);
    let mut by_task: HashMap<Option<String>, TaskAccum> = HashMap::new();
    for node in emission_order.iter().copied() {
        if !agent_for_node.contains_key(&node.id) || !node_emits_visible_content(node, indices) {
            continue;
        }
        let task_id = prop_str(node, a2a::TASK_ID)
            .or_else(|| resolve_task_id_for_sequence_node(node, indices));
        let entry = by_task
            .entry(task_id)
            .or_insert_with(|| (Vec::new(), Vec::new(), Vec::new()));
        if let Some(agent) = agent_for_node.get(&node.id) {
            let a = sanitize_participant(agent);
            if !entry.0.contains(&a) {
                entry.0.push(a);
            }
        }
        match indices
            .label_by_node
            .get(node.id.as_str())
            .copied()
            .flatten()
        {
            Some(GraphNodeLabel::LlmCall) => {
                let model = prop_str(node, a2a::MODEL).unwrap_or_else(|| "unknown".to_string());
                let llm = sanitize_participant(&format!("LLM {model}"));
                if !entry.1.contains(&llm) {
                    entry.1.push(llm);
                }
            }
            Some(GraphNodeLabel::ToolCall) => {
                let tool_name_raw = node
                    .properties
                    .get(a2a::TOOL_NAME)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !A2A_MEDIATOR_TOOLS.contains(&tool_name_raw) {
                    let tool = sanitize_participant(strip_tool_prefix(tool_name_raw));
                    if !tool.is_empty() && !entry.2.contains(&tool) {
                        entry.2.push(tool);
                    }
                }
            }
            _ => {}
        }
    }
    let mut result = HashMap::new();
    for (task_id, (agents, llms, tools)) in by_task {
        // First = executing agent for this task. Never span multiple agents.
        let first = if participants.has_user && agents.is_empty() {
            Some("User".to_string())
        } else {
            agents.first().cloned()
        }
        .or_else(|| participants.agents.first().map(|a| sanitize_participant(a)))
        .unwrap_or_else(|| "User".into());
        // Last = tools/llms of this agent, or the agent itself. Never span to a different agent.
        // When multiple agents share a task_id (e.g. coordinator+worker), span only the first.
        let last = if agents.len() > 1 {
            agents.first().cloned().unwrap_or_else(|| first.clone())
        } else {
            tools
                .last()
                .or_else(|| llms.last())
                .or_else(|| agents.first())
                .cloned()
                .unwrap_or_else(|| first.clone())
        };
        result.insert(task_id, (first, last));
    }
    result
}

/// Build a map from node id to sanitized agent package name for every node
/// that has a complete chain: activity → AgentRuntimeInstance → AgentBoot → AgentArchive.
fn build_agent_for_node_map(
    graph: &ExportedGraph,
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
    activity_cache: &mut HashMap<String, Option<String>>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for node in &graph.nodes {
        if let Some(name) =
            resolve_agent_package_for_node_indices(indices, resolution, node, activity_cache)
        {
            map.insert(node.id.clone(), sanitize_participant(&name));
        }
    }
    map
}

/// Resolve archive path for a node using indices and resolution maps (O(1) / O(degree) lookups).
fn resolve_agent_package_for_node_indices(
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
    node: &ExportedNode,
    activity_cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    let activity_id = find_executing_activity_for_node_indices(
        indices,
        resolution,
        node.id.as_str(),
        node.label.as_str(),
        activity_cache,
    )?;
    let agent_id = *resolution.activity_to_agent.get(activity_id.as_str())?;
    let agent_node = indices.nodes_by_id.get(agent_id)?;
    prop_str(agent_node, a2a::ARCHIVE_PATH)
}

/// Find the activity that executed this node by scanning only edges touching the node.
/// Memoized via `activity_cache` to avoid O(N²) redundant traversals across nodes.
fn find_executing_activity_for_node_indices(
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
    node_id: &str,
    node_label: &str,
    activity_cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if let Some(cached) = activity_cache.get(node_id) {
        return cached.clone();
    }
    let result =
        find_executing_activity_impl(indices, resolution, node_id, node_label, activity_cache);
    activity_cache.insert(node_id.to_string(), result.clone());
    result
}

fn find_executing_activity_impl(
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
    node_id: &str,
    node_label: &str,
    activity_cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if resolution.activity_to_agent.contains_key(node_id) {
        return Some(node_id.to_string());
    }
    let label = indices
        .label_by_node
        .get(node_id)
        .copied()
        .flatten()
        .or_else(|| GraphNodeLabel::parse(node_label));
    let edges_to = indices.edges_by_to.get(node_id).into_iter().flatten();
    let edges_from = indices.edges_by_from.get(node_id).into_iter().flatten();
    for edge in edges_to.chain(edges_from) {
        let found = match label {
            Some(GraphNodeLabel::Message) => {
                if edge.to == node_id
                    && (edge_relation_matches(&edge.relation, EDGE_WAS_RECEIVED_BY)
                        || edge_relation_matches(&edge.relation, EDGE_WAS_SPAWNED_BY))
                    && resolution
                        .activity_to_agent
                        .contains_key(edge.from.as_str())
                {
                    Some(edge.from.to_string())
                } else if edge.from == node_id
                    && edge_relation_matches(&edge.relation, EDGE_WAS_EMITTED_BY)
                    && resolution.activity_to_agent.contains_key(edge.to.as_str())
                {
                    Some(edge.to.to_string())
                } else if edge.to == node_id
                    && edge_relation_matches(&edge.relation, semantic_labels::WAS_CONSUMED_BY)
                {
                    indices
                        .nodes_by_id
                        .get(edge.from.as_str())
                        .and_then(|consumer| {
                            find_executing_activity_for_node_indices(
                                indices,
                                resolution,
                                &consumer.id,
                                &consumer.label,
                                activity_cache,
                            )
                        })
                } else {
                    None
                }
            }
            Some(GraphNodeLabel::LlmCall) => {
                if edge.to == node_id
                    && edge_relation_matches(&edge.relation, EDGE_WAS_INVOKED_BY)
                    && resolution
                        .activity_to_agent
                        .contains_key(edge.from.as_str())
                {
                    Some(edge.from.to_string())
                } else {
                    None
                }
            }
            Some(GraphNodeLabel::PromptRejected) => {
                if edge.to == node_id
                    && edge_relation_matches(&edge.relation, EDGE_WAS_INVOKED_BY)
                    && resolution
                        .activity_to_agent
                        .contains_key(edge.from.as_str())
                {
                    Some(edge.from.to_string())
                } else {
                    None
                }
            }
            Some(GraphNodeLabel::ToolCall) => {
                if edge.to == node_id
                    && edge_relation_matches(&edge.relation, EDGE_WAS_EXECUTED_BY)
                    && resolution
                        .activity_to_agent
                        .contains_key(edge.from.as_str())
                {
                    Some(edge.from.to_string())
                } else {
                    None
                }
            }
            Some(GraphNodeLabel::TaskState) => {
                if edge.to == node_id
                    && edge_relation_matches(&edge.relation, EDGE_WAS_UPDATED_BY)
                    && resolution
                        .activity_to_agent
                        .contains_key(edge.from.as_str())
                {
                    Some(edge.from.to_string())
                } else {
                    None
                }
            }
            Some(GraphNodeLabel::Artifact) => {
                if edge.from == node_id
                    && edge_relation_matches(&edge.relation, EDGE_WAS_GENERATED_BY)
                    && resolution.activity_to_agent.contains_key(edge.to.as_str())
                {
                    Some(edge.to.to_string())
                } else {
                    None
                }
            }
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Build turn number per node. User messages get turns 1, 2, 3... by event_order.
/// Replies and processing nodes inherit the turn of the user message that triggered them.
/// Ensures replies appear before the next user message even when event_order is later.
fn build_turn_by_node(graph: &ExportedGraph, indices: &GraphIndices<'_>) -> HashMap<String, u32> {
    let mut user_msgs: Vec<&ExportedNode> = graph
        .nodes
        .iter()
        .filter(|n| {
            indices.label_by_node.get(n.id.as_str()).copied().flatten()
                == Some(GraphNodeLabel::Message)
                && is_user_message(n)
        })
        .collect();
    user_msgs.sort_by_key(|n| (n.event_order.unwrap_or(u64::MAX), &n.id));
    let turn_by_user: HashMap<&str, u32> = user_msgs
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), (i + 1) as u32))
        .collect();

    let user_msg_order: Vec<u64> = user_msgs
        .iter()
        .map(|n| n.event_order.unwrap_or(0))
        .collect();

    let mut turn_by_node: HashMap<String, u32> = HashMap::new();
    for node in &graph.nodes {
        let turn = turn_for_node(node, indices, &turn_by_user, &user_msg_order);
        turn_by_node.insert(node.id.clone(), turn);
    }
    turn_by_node
}

/// Resolve turn for a node. Replies inherit from the MessageProcessing that emitted them.
fn turn_for_node(
    node: &ExportedNode,
    indices: &GraphIndices<'_>,
    turn_by_user: &HashMap<&str, u32>,
    user_msg_order: &[u64],
) -> u32 {
    if let Some(&t) = turn_by_user.get(node.id.as_str()) {
        return t;
    }
    let label = indices
        .label_by_node
        .get(node.id.as_str())
        .copied()
        .flatten();
    let order = node.event_order.unwrap_or(0);
    if label == Some(GraphNodeLabel::Message) {
        // Assistant message: reply -[WAS_EMITTED_BY]-> MP. MP -[WAS_RECEIVED_BY]-> user_msg.
        for edge in indices
            .edges_by_from
            .get(node.id.as_str())
            .into_iter()
            .flatten()
        {
            if edge_relation_matches(&edge.relation, EDGE_WAS_EMITTED_BY) {
                let mp = edge.to.as_str();
                for e2 in indices.edges_by_from.get(mp).into_iter().flatten() {
                    if edge_relation_matches(&e2.relation, EDGE_WAS_RECEIVED_BY)
                        && let Some(&t) = turn_by_user.get(e2.to.as_str())
                    {
                        return t;
                    }
                }
            }
        }
    }
    if label == Some(GraphNodeLabel::LlmCall) || label == Some(GraphNodeLabel::ToolCall) {
        for edge in indices
            .edges_by_to
            .get(node.id.as_str())
            .into_iter()
            .flatten()
        {
            if edge_relation_matches(&edge.relation, EDGE_WAS_INVOKED_BY)
                || edge_relation_matches(&edge.relation, EDGE_WAS_EXECUTED_BY)
            {
                let mp = edge.from.as_str();
                for e2 in indices.edges_by_from.get(mp).into_iter().flatten() {
                    if edge_relation_matches(&e2.relation, EDGE_WAS_RECEIVED_BY)
                        && let Some(&t) = turn_by_user.get(e2.to.as_str())
                    {
                        return t;
                    }
                }
            }
        }
    }
    if label == Some(GraphNodeLabel::Artifact) {
        for edge in indices
            .edges_by_from
            .get(node.id.as_str())
            .into_iter()
            .flatten()
        {
            if edge_relation_matches(&edge.relation, EDGE_WAS_GENERATED_BY) {
                let activity = edge.to.as_str();
                if let Some(act_node) = indices.nodes_by_id.get(activity) {
                    return turn_for_node(act_node, indices, turn_by_user, user_msg_order);
                }
            }
        }
    }
    // Fallback: turn of last user message with event_order <= this node's.
    let idx = user_msg_order
        .iter()
        .rposition(|&o| o <= order)
        .unwrap_or(0);
    (idx + 1) as u32
}

/// Order for emission: turn first (so replies appear before next user msg), then first user message,
/// then event_order, then type priority, then id.
#[allow(clippy::too_many_arguments)]
fn cmp_emission_order_with_turn(
    a_order: Option<u64>,
    a_id: &str,
    a_label: Option<GraphNodeLabel>,
    a_turn: Option<u32>,
    b_order: Option<u64>,
    b_id: &str,
    b_label: Option<GraphNodeLabel>,
    b_turn: Option<u32>,
    first_user_msg_id: Option<&str>,
) -> std::cmp::Ordering {
    let a_turn = a_turn.unwrap_or(u32::MAX);
    let b_turn = b_turn.unwrap_or(u32::MAX);
    a_turn
        .cmp(&b_turn)
        .then_with(|| {
            if let Some(fid) = first_user_msg_id {
                match (a_id == fid, b_id == fid) {
                    (true, false) => return std::cmp::Ordering::Less,
                    (false, true) => return std::cmp::Ordering::Greater,
                    _ => {}
                }
            }
            std::cmp::Ordering::Equal
        })
        .then_with(|| match (a_order, b_order) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        })
        .then_with(|| {
            node_type_emission_priority_enum(a_label)
                .cmp(&node_type_emission_priority_enum(b_label))
        })
        .then_with(|| a_id.cmp(b_id))
}

/// Lower = earlier in diagram. Message first so initial user message appears before status notes.
fn node_type_emission_priority_enum(label: Option<GraphNodeLabel>) -> u8 {
    match label {
        Some(GraphNodeLabel::Message) => 0,
        Some(GraphNodeLabel::TaskState) => 1,
        Some(GraphNodeLabel::LlmCall) => 2,
        Some(GraphNodeLabel::ToolCall) => 3,
        Some(GraphNodeLabel::PromptRejected) => 4,
        Some(GraphNodeLabel::Artifact) => 5,
        _ => 6,
    }
}

// ── Node → sequence message mapping ─────────────────────────────────────────

/// Emit the appropriate sequence diagram line(s) for a single node.
/// Sender for messages is always the resolved `agent` from the graph (no override).
/// `last_activated_agent` is used only for Mermaid activation pairing (when to emit `-`).
fn emit_node(
    out: &mut String,
    node: &ExportedNode,
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
    participants: &Participants,
    agent: &str,
    last_activated_agent: &mut Option<String>,
) {
    match indices
        .label_by_node
        .get(node.id.as_str())
        .copied()
        .flatten()
    {
        Some(GraphNodeLabel::Message) => {
            emit_message(out, node, indices, resolution, agent, last_activated_agent)
        }
        Some(GraphNodeLabel::LlmCall) => {
            ensure_agent_activated(out, agent, last_activated_agent);
            emit_llm_call(out, node, agent)
        }
        Some(GraphNodeLabel::PromptRejected) => {
            ensure_agent_activated(out, agent, last_activated_agent);
            emit_prompt_rejected(out, node, agent)
        }
        Some(GraphNodeLabel::ToolCall) => {
            let tool_name_raw = node
                .properties
                .get(a2a::TOOL_NAME)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !A2A_MEDIATOR_TOOLS.contains(&tool_name_raw) {
                ensure_agent_activated(out, agent, last_activated_agent);
                emit_tool_call(out, node, indices, participants, agent)
            }
        }
        Some(GraphNodeLabel::Artifact) => {
            ensure_agent_activated(out, agent, last_activated_agent);
            emit_artifact(out, node, agent)
        }
        // MessageProcessing, TaskExecution, etc. are structural — skip.
        _ => {}
    }
}

/// Resolve the recipient of an emitted message from provenance edges.
///
/// 1. **WAS_RECEIVED_BY**: `MessageProcessing --WAS_RECEIVED_BY--> Message` — another agent's
///    activity received it. Use that agent as recipient.
/// 2. **WAS_CONSUMED_BY**: `ToolCall --WAS_CONSUMED_BY--> Message` — a tool consumed it as input.
///    Use the tool name as recipient (e.g. claude_dev, not User).
/// 3. **Delegation fallback**: If the sender is a delegation target (e.g. claude_session_demo
///    emitting Requirements/Plan), the invoking agent (who called internal_a2a) is the recipient.
/// 4. Otherwise we assume the message goes to the User.
fn resolve_message_recipient(
    message_id: &str,
    sender_agent: &str,
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
) -> Option<String> {
    for edge in indices.edges_by_to.get(message_id).into_iter().flatten() {
        if edge_relation_matches(&edge.relation, EDGE_WAS_RECEIVED_BY) {
            let receiver_activity = edge.from.as_str();
            let agent_id = resolution.activity_to_agent.get(receiver_activity)?;
            let agent_node = indices.nodes_by_id.get(agent_id)?;
            let archive_path = prop_str(agent_node, a2a::ARCHIVE_PATH)?;
            let agent_type = indices
                .nodes_by_id
                .get(agent_id)
                .and_then(|n| prop_str(n, a2a::AGENT_TYPE))
                .unwrap_or_default();
            if is_infrastructure_agent(&agent_type) {
                continue;
            }
            let recipient = sanitize_participant(&archive_path);
            if recipient != sender_agent {
                return Some(recipient);
            }
        } else if edge_relation_matches(&edge.relation, semantic_labels::WAS_CONSUMED_BY) {
            // ToolCall consumed this message (e.g. prompt to claude_dev). Recipient is the tool.
            let tool_call_id = edge.from.as_str();
            if let Some(tool_node) = indices.nodes_by_id.get(tool_call_id)
                && let Some(tool_name) = prop_str(tool_node, a2a::TOOL_NAME)
            {
                let tool_participant = sanitize_participant(strip_tool_prefix(&tool_name));
                return Some(tool_participant);
            }
        }
    }

    // Delegation fallback: sender is a delegate; recipient is the invoking agent.
    // delegation_edges: ToolCall -[WAS_DELEGATED_TO]-> DelegationTarget (edge.from = ToolCall).
    for edge in &indices.delegation_edges {
        let target_node = indices.nodes_by_id.get(edge.to.as_str())?;
        let delegation_target = prop_str(target_node, a2a::DELEGATION_TARGET)?;
        let dt_sanitized = sanitize_participant(&delegation_target);
        let sender_matches = agents_match(sender_agent, &dt_sanitized);
        if !sender_matches {
            continue;
        }
        let agent_id = resolve_invoker_for_delegation(indices, resolution, edge.from.as_str())?;
        let agent_node = indices.nodes_by_id.get(agent_id)?;
        let archive_path = prop_str(agent_node, a2a::ARCHIVE_PATH)?;
        let agent_type = indices
            .nodes_by_id
            .get(agent_id)
            .and_then(|n| prop_str(n, a2a::AGENT_TYPE))
            .unwrap_or_default();
        if is_infrastructure_agent(&agent_type) {
            continue;
        }
        let recipient = sanitize_participant(&archive_path);
        if recipient != sender_agent {
            return Some(recipient);
        }
    }
    None
}

/// Resolve the agent that invoked a delegation (ToolCall). The ToolCall is executed by
/// MessageProcessing/TaskExecution; activity_to_agent maps that parent activity to the agent.
fn resolve_invoker_for_delegation<'a>(
    indices: &GraphIndices<'a>,
    resolution: &ResolutionMaps<'a>,
    tool_call_id: &str,
) -> Option<&'a str> {
    let parent_activity = indices
        .edges_by_to
        .get(tool_call_id)
        .into_iter()
        .flatten()
        .find(|e| edge_relation_matches(&e.relation, EDGE_WAS_EXECUTED_BY))
        .map(|e| e.from.as_str())?;
    resolution.activity_to_agent.get(parent_activity).copied()
}

/// For a role=user message received by `recipient_agent`: if the recipient is a
/// delegation target, the actual sender is the invoking agent (persona), not User.
/// delegation_edges: ToolCall -[WAS_DELEGATED_TO]-> DelegationTarget (edge.from = ToolCall).
fn resolve_user_message_sender(
    recipient_agent: &str,
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
) -> Option<String> {
    for edge in &indices.delegation_edges {
        let target_node = indices.nodes_by_id.get(edge.to.as_str())?;
        let delegation_target = prop_str(target_node, a2a::DELEGATION_TARGET)?;
        let dt_sanitized = sanitize_participant(&delegation_target);
        let recipient_matches = agents_match(recipient_agent, &dt_sanitized);
        if !recipient_matches {
            continue;
        }
        let agent_id = resolve_invoker_for_delegation(indices, resolution, edge.from.as_str())?;
        let agent_node = indices.nodes_by_id.get(agent_id)?;
        let archive_path = prop_str(agent_node, a2a::ARCHIVE_PATH)?;
        let agent_type = indices
            .nodes_by_id
            .get(agent_id)
            .and_then(|n| prop_str(n, a2a::AGENT_TYPE))
            .unwrap_or_default();
        if is_infrastructure_agent(&agent_type) {
            continue;
        }
        let invoker = sanitize_participant(&archive_path);
        if invoker != recipient_agent {
            return Some(invoker);
        }
    }
    None
}

/// Ensure the agent has an activation bar before LLM/tool calls. When the user message was skipped
/// (e.g. empty content) or we're in a later task, the agent would otherwise show fragmented
/// activations (one per call). Emit explicit `activate` so the agent has one continuous bar.
fn ensure_agent_activated(
    out: &mut String,
    agent: &str,
    last_activated_agent: &mut Option<String>,
) {
    if last_activated_agent.as_deref() != Some(agent) {
        let _ = writeln!(out, "    activate {agent}");
        *last_activated_agent = Some(agent.to_string());
    }
}

/// Emit a `User->>Agent` or `Agent->>Recipient` arrow for a Message node.
/// Sender is resolved from the graph (WAS_DELEGATED_TO); no fallbacks.
fn emit_message(
    out: &mut String,
    node: &ExportedNode,
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
    agent: &str,
    last_activated_agent: &mut Option<String>,
) {
    let role = node
        .properties
        .get(a2a::ROLE)
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let normalized = normalize_role(role);
    if normalized.is_empty() {
        return;
    }
    let content = super::extract_content_preview(
        node.properties.get(a2a::CONTENT),
        SEQUENCE_CONTENT_PREVIEW_LEN,
    );
    let escaped = match normalized.as_str() {
        "assistant" | "agent" => {
            // Always emit the reply arrow so each task shows its lifecycle (activation → work →
            // reply → deactivation). Use placeholder when content is empty.
            escape_sequence_text(if content.is_empty() { "…" } else { &content })
        }
        _ if content.is_empty() => return,
        _ => escape_sequence_text(&content),
    };

    match normalized.as_str() {
        "user" => {
            // If recipient is a delegate, sender is the invoking agent (persona), not User.
            let sender = resolve_user_message_sender(agent, indices, resolution)
                .unwrap_or_else(|| "User".to_string());
            let _ = writeln!(out, "    {sender}->>+{agent}: {escaped}");
            *last_activated_agent = Some(agent.to_string());
        }
        "assistant" | "agent" => {
            let recipient = resolve_message_recipient(&node.id, agent, indices, resolution)
                .unwrap_or_else(|| "User".to_string());
            let deactivate = last_activated_agent.as_deref() == Some(agent);
            let _ = if deactivate {
                writeln!(out, "    {agent}->>-{recipient}: {escaped}")
            } else {
                writeln!(out, "    {agent}->>{recipient}: {escaped}")
            };
            if deactivate {
                *last_activated_agent = None;
            }
        }
        other => {
            let _ = writeln!(
                out,
                "    Note over {agent}: {other}: {}",
                escape_note_content(&content)
            );
        }
    }
}

/// Emit Agent->>LLM and LLM-->>Agent arrows for an LLM call, with token usage on the response arrow.
fn emit_llm_call(out: &mut String, node: &ExportedNode, agent: &str) {
    let model = prop_str(node, a2a::MODEL).unwrap_or_else(|| "unknown".to_string());
    let llm = sanitize_participant(&format!("LLM {model}"));
    let function_name = prop_str(node, a2a::FUNCTION_NAME);
    let request_label = function_name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "call".to_string());

    // Agent -> LLM (request)
    let _ = writeln!(
        out,
        "    {agent}->>+{llm}: {}",
        escape_sequence_text(&request_label)
    );

    // LLM -> Agent (response with timing and token usage on arrow)
    let mut response = String::new();
    if let Some(dur) = prop_str(node, a2a::DURATION_MS) {
        response.push_str(&format!("{dur}ms"));
    }
    let outcome = NodeActivityOutcome::from_props(&node.properties);
    let failed = outcome == Some(NodeActivityOutcome::Failed);
    if outcome == Some(NodeActivityOutcome::Success) {
        if !response.is_empty() {
            response.push(' ');
        }
        response.push('✓');
    } else if failed {
        if !response.is_empty() {
            response.push(' ');
        }
        response.push('✗');
        if let Some(err) = extract_error_preview(node) {
            response.push_str(&format!(" {err}"));
        }
    }
    if let Some(usage) = llm_usage_summary(node) {
        if !response.is_empty() {
            response.push(' ');
        }
        response.push_str(&usage);
    }
    if response.is_empty() {
        response.push_str("done");
    }
    if failed {
        let _ = writeln!(
            out,
            "    {llm}{}{agent}: {}",
            MERMAID_FAILURE_ARROW,
            escape_sequence_text(&response)
        );
        let _ = writeln!(out, "    deactivate {llm}");
    } else {
        let _ = writeln!(
            out,
            "    {llm}-->>-{agent}: {}",
            escape_sequence_text(&response)
        );
    }
}

/// Emit a `Note over Agent` for a BAML prompt rejection/provenance failure marker.
/// Truncates the reason to avoid Mermaid parse errors from very long Note content.
fn emit_prompt_rejected(out: &mut String, node: &ExportedNode, agent: &str) {
    let reason = prop_str(node, a2a::REASON).unwrap_or_else(|| "unknown reason".to_string());
    let truncated = super::truncate_str(&reason, SEQUENCE_ERROR_PREVIEW_LEN);
    let note = format!("✗ BAML rejection: {truncated}");
    let _ = writeln!(out, "    Note over {agent}: {}", escape_note_content(&note));
}

const A2A_MEDIATOR_TOOLS: [&str; 2] = ["system/internal_a2a", "system/a2a"];

/// Emit `Agent->>Tool` and `Tool-->>Agent` arrows for a ToolCall node.
fn emit_tool_call(
    out: &mut String,
    node: &ExportedNode,
    indices: &GraphIndices<'_>,
    _participants: &Participants,
    agent: &str,
) {
    let tool_name_raw = node
        .properties
        .get(a2a::TOOL_NAME)
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let tool_raw = strip_tool_prefix(tool_name_raw);
    let tool_participant = sanitize_participant(tool_raw);
    let args_summary = find_tool_args(indices, &node.id);

    // Agent → Tool (request).
    let send_label = if args_summary.is_empty() || args_summary == "(empty)" {
        "call".to_string()
    } else {
        escape_sequence_text(&args_summary)
    };
    // Activate tool on request.
    let _ = writeln!(out, "    {agent}->>+{tool_participant}: {send_label}");

    // Tool → Agent (response). Use cross-headed arrow (--x) for failures.
    let mut response = String::new();
    if let Some(dur) = prop_str(node, a2a::DURATION_MS) {
        response.push_str(&format!("{dur}ms"));
    }
    let outcome = NodeActivityOutcome::from_props(&node.properties);
    let failed = outcome == Some(NodeActivityOutcome::Failed);
    if outcome == Some(NodeActivityOutcome::Success) {
        if !response.is_empty() {
            response.push(' ');
        }
        response.push('✓');
    } else if failed {
        if !response.is_empty() {
            response.push(' ');
        }
        response.push('✗');
        if let Some(err) = extract_error_preview(node) {
            response.push_str(&format!(" {err}"));
        }
    }
    if response.is_empty() {
        response.push_str("done");
    }
    if failed {
        let _ = writeln!(
            out,
            "    {tool_participant}{}{agent}: {}",
            MERMAID_FAILURE_ARROW,
            escape_sequence_text(&response)
        );
        let _ = writeln!(out, "    deactivate {tool_participant}");
        if let Some(err) = extract_error_preview(node) {
            let _ = writeln!(
                out,
                "    Note over {tool_participant},{agent}: ✗ {}",
                escape_note_content(&err)
            );
        }
    } else {
        // Deactivate tool on response.
        let _ = writeln!(
            out,
            "    {tool_participant}-->>-{agent}: {}",
            escape_sequence_text(&response)
        );
    }
}

/// Emit a `Note over Agent` for an artifact generated during task execution.
fn emit_artifact(out: &mut String, node: &ExportedNode, agent: &str) {
    let artifact_id = prop_str(node, a2a::ARTIFACT_ID);
    let artifact_type = prop_str(node, a2a::ARTIFACT_TYPE);
    let note = match (artifact_type, artifact_id) {
        (Some(kind), Some(id)) => format!("Artifact {kind} ({id})"),
        (Some(kind), None) => format!("Artifact {kind}"),
        (None, Some(id)) => format!("Artifact ({id})"),
        (None, None) => "Artifact generated".to_string(),
    };
    let _ = writeln!(out, "    Note over {agent}: {}", escape_note_content(&note));
}

/// Relation comparison: graph may store relation with different casing/whitespace.
fn edge_relation_matches(edge_relation: &str, expected: &str) -> bool {
    let t = edge_relation.trim();
    t == expected || t.eq_ignore_ascii_case(expected)
}

// ── Participant extraction ──────────────────────────────────────────────────

/// Identified participants for the sequence diagram.
struct Participants {
    has_user: bool,
    /// Agent display names from the graph: AgentRuntimeInstance → AgentBoot → AgentArchive (a2a:archive_path).
    agents: Vec<String>,
    /// LLM participants: "LLM {model}" per unique model.
    llms: Vec<String>,
    /// Tool short names (prefix-stripped, deduplicated).
    tools: Vec<String>,
}

/// Walk the graph once to discover all participants. Agent names from
/// No fallbacks: only agents with a complete chain appear.
fn extract_participants(
    graph: &ExportedGraph,
    indices: &GraphIndices<'_>,
    resolution: &ResolutionMaps<'_>,
    activity_cache: &mut HashMap<String, Option<String>>,
) -> Participants {
    let mut has_user = false;
    let mut agents: Vec<String> = Vec::new();
    let mut seen_agents: HashSet<String> = HashSet::new();
    let mut agent_first_order: HashMap<String, u64> = HashMap::new();
    let mut llms: Vec<String> = Vec::new();
    let mut seen_llms: HashSet<String> = HashSet::new();
    let mut llm_first_order: HashMap<String, u64> = HashMap::new();
    let mut tools: Vec<String> = Vec::new();
    let mut seen_tools: HashSet<String> = HashSet::new();
    let mut tool_first_order: HashMap<String, u64> = HashMap::new();

    for node in &graph.nodes {
        match indices
            .label_by_node
            .get(node.id.as_str())
            .copied()
            .flatten()
        {
            Some(GraphNodeLabel::Message) => {
                let role = node
                    .properties
                    .get(a2a::ROLE)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let normalized = normalize_role(role);
                if matches!(normalized.as_str(), "user" | "assistant" | "agent") {
                    has_user = true;
                }
            }
            Some(GraphNodeLabel::AgentRuntimeInstance) => {
                let agent_type = node
                    .properties
                    .get(a2a::AGENT_TYPE)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if is_infrastructure_agent(agent_type) {
                    continue;
                }
                if let Some(archive_path) = prop_str(node, a2a::ARCHIVE_PATH)
                    && seen_agents.insert(archive_path.clone())
                {
                    agents.push(archive_path);
                }
            }
            Some(GraphNodeLabel::LlmCall) => {
                let model = prop_str(node, a2a::MODEL).unwrap_or_else(|| "unknown".to_string());
                let llm_name = format!("LLM {model}");
                if seen_llms.insert(llm_name.clone()) {
                    llms.push(llm_name.clone());
                }
                if let Some(order) = node.event_order {
                    llm_first_order
                        .entry(llm_name)
                        .and_modify(|current| *current = (*current).min(order))
                        .or_insert(order);
                }
            }
            Some(GraphNodeLabel::ToolCall) => {
                let tool_name_raw = node
                    .properties
                    .get(a2a::TOOL_NAME)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if A2A_MEDIATOR_TOOLS.contains(&tool_name_raw) {
                    continue;
                }
                let tool = strip_tool_prefix(tool_name_raw);
                if tool.is_empty() {
                    continue;
                }
                if seen_tools.insert(tool.to_string()) {
                    tools.push(tool.to_string());
                }
                if let Some(order) = node.event_order {
                    tool_first_order
                        .entry(tool.to_string())
                        .and_modify(|current| *current = (*current).min(order))
                        .or_insert(order);
                }
            }
            _ => {}
        }

        if let Some(order) = node.event_order
            && let Some(agent_name) =
                resolve_agent_package_for_node_indices(indices, resolution, node, activity_cache)
        {
            agent_first_order
                .entry(agent_name)
                .and_modify(|current| *current = (*current).min(order))
                .or_insert(order);
        }
    }

    agents.sort_by(|a, b| {
        let a_order = agent_first_order.get(a);
        let b_order = agent_first_order.get(b);
        match (a_order, b_order) {
            (Some(x), Some(y)) => x.cmp(y).then_with(|| a.cmp(b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });
    llms.sort_by(|a, b| {
        let a_order = llm_first_order.get(a);
        let b_order = llm_first_order.get(b);
        match (a_order, b_order) {
            (Some(x), Some(y)) => x.cmp(y).then_with(|| a.cmp(b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });
    tools.sort_by(|a, b| {
        let a_order = tool_first_order.get(a);
        let b_order = tool_first_order.get(b);
        match (a_order, b_order) {
            (Some(x), Some(y)) => x.cmp(y).then_with(|| a.cmp(b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });

    Participants {
        has_user,
        agents,
        llms,
        tools,
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Normalize a role string: strip `ROLE_` prefix and lowercase.
fn normalize_role(raw: &str) -> String {
    let stripped = raw.strip_prefix("ROLE_").unwrap_or(raw);
    stripped.to_lowercase()
}

/// Infer message direction role from provenance edges when `a2a:role` is absent.
/// Extract a string property from a node (handles String, Number, Bool).
fn prop_str(node: &ExportedNode, key: &str) -> Option<String> {
    node.properties.get(key).and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

/// Build a compact usage summary from LLM token properties when present.
fn llm_usage_summary(node: &ExportedNode) -> Option<String> {
    let prompt = prop_str(node, a2a::USAGE_PROMPT_TOKENS);
    let completion = prop_str(node, a2a::USAGE_COMPLETION_TOKENS);
    let total = prop_str(node, a2a::USAGE_TOTAL_TOKENS);
    match (prompt, completion, total) {
        (Some(input), Some(output), Some(total)) => {
            Some(format!("in:{input}, out:{output}, total:{total}"))
        }
        (Some(input), Some(output), None) => Some(format!("in:{input}, out:{output}")),
        _ => None,
    }
}

/// Heuristic: skip the synthetic "previous" status companion that shares the
/// same timestamp as a transition node and echoes that transition's old status.
/// Strip known tool name prefixes for brevity.
fn strip_tool_prefix(name: &str) -> &str {
    name.strip_prefix("support/")
        .or_else(|| name.strip_prefix("system/"))
        .unwrap_or(name)
}

/// True if two agent identifiers refer to the same agent (handles package vs versioned forms).
fn agents_match(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}_")) || b.starts_with(&format!("{a}_"))
}

/// Infrastructure agent types that should not appear as sequence diagram
/// participants. These are control-plane agents, not conversation actors.
fn is_infrastructure_agent(agent_type: &str) -> bool {
    matches!(agent_type, agent_types::RUNNER | agent_types::CLIENT)
}

fn find_tool_args(indices: &GraphIndices<'_>, tool_call_id: &str) -> String {
    for edge in indices
        .edges_by_from
        .get(tool_call_id)
        .into_iter()
        .flatten()
    {
        if edge.relation == "WAS_USED_BY"
            && let Some(args_node) = indices.nodes_by_id.get(edge.to.as_str())
            && args_node.label == "ToolArgs"
        {
            return super::summarize_args(
                args_node.properties.get(a2a::ARGS),
                SEQUENCE_ARGS_SUMMARY_LEN,
            );
        }
    }
    String::new()
}

/// Sanitize a participant name for Mermaid sequence diagrams.
///
/// Mermaid participant names must be alphanumeric (with underscores).
fn sanitize_participant(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Extract a truncated error message preview from a node.
///
/// The normalizer stores the error string as `a2a:error` on the activity node.
/// Fallback: `metadata.error` for legacy/test nodes that use nested metadata.
fn extract_error_preview(node: &ExportedNode) -> Option<String> {
    let raw = node
        .properties
        .get(a2a::ERROR)
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| super::extract_metadata_field(node.properties.get(a2a::METADATA), "error"));
    let raw = raw.filter(|s| !s.is_empty())?;
    Some(super::truncate_str(&raw, SEQUENCE_ERROR_PREVIEW_LEN))
}

/// Escape text for use in Mermaid sequence diagram arrow labels.
fn escape_sequence_text(s: &str) -> String {
    s.replace('"', "'")
        .replace('<', "‹")
        .replace('>', "›")
        .replace('\n', " ")
}

/// Escape and wrap Note content in double quotes so reserved words (e.g. "end") and
/// colons do not break the Mermaid parser.
fn escape_note_content(s: &str) -> String {
    format!("\"{}\"", escape_sequence_text(s))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::graph_export::{ExportScope, ExportedEdge, ExportedGraph, ExportedNode, enrich};

    fn msg_node(id: &str, role: &str, content: &str, order: Option<u64>) -> ExportedNode {
        let mut props = HashMap::new();
        props.insert(
            a2a::ROLE.to_string(),
            serde_json::Value::String(role.to_string()),
        );
        props.insert(
            a2a::CONTENT.to_string(),
            serde_json::Value::String(content.to_string()),
        );
        ExportedNode {
            id: id.to_string(),
            label: "Message".to_string(),
            display_name: format!("📩 {role}: {content}"),
            properties: props,
            event_order: order,
        }
    }

    fn agent_node(id: &str, agent_type: &str) -> ExportedNode {
        let mut props = HashMap::new();
        props.insert(
            a2a::AGENT_TYPE.to_string(),
            serde_json::Value::String(agent_type.to_string()),
        );
        props.insert(
            a2a::ARCHIVE_PATH.to_string(),
            serde_json::Value::String(agent_type.to_string()),
        );
        ExportedNode {
            id: id.to_string(),
            label: "AgentRuntimeInstance".to_string(),
            display_name: format!("🖥️ Agent {agent_type}"),
            properties: props,
            event_order: None,
        }
    }

    fn llm_node(id: &str, model: &str, duration_ms: u64, order: Option<u64>) -> ExportedNode {
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String(model.to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(duration_ms));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Success"),
        );
        ExportedNode {
            id: id.to_string(),
            label: "LlmCall".to_string(),
            display_name: format!("🤖 LLM {model} {duration_ms}ms ✅"),
            properties: props,
            event_order: order,
        }
    }

    fn tool_node(id: &str, tool_name: &str, duration_ms: u64, order: Option<u64>) -> ExportedNode {
        let mut props = HashMap::new();
        props.insert(
            a2a::TOOL_NAME.to_string(),
            serde_json::Value::String(tool_name.to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(duration_ms));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Success"),
        );
        ExportedNode {
            id: id.to_string(),
            label: "ToolCall".to_string(),
            display_name: format!("🔧 {tool_name}"),
            properties: props,
            event_order: order,
        }
    }

    fn args_node(id: &str, args_json: &str) -> ExportedNode {
        let mut props = HashMap::new();
        props.insert(
            a2a::ARGS.to_string(),
            serde_json::Value::String(args_json.to_string()),
        );
        ExportedNode {
            id: id.to_string(),
            label: "ToolArgs".to_string(),
            display_name: "📋 Args".to_string(),
            properties: props,
            event_order: None,
        }
    }

    fn delegation_target_node(id: &str, agent_package: &str) -> ExportedNode {
        let mut props = HashMap::new();
        props.insert(
            a2a::DELEGATION_TARGET.to_string(),
            serde_json::Value::String(agent_package.to_string()),
        );
        ExportedNode {
            id: id.to_string(),
            label: "DelegationTarget".to_string(),
            display_name: format!("DelegationTarget {agent_package}"),
            properties: props,
            event_order: None,
        }
    }

    fn edge(from: &str, rel: &str, to: &str) -> ExportedEdge {
        ExportedEdge {
            from: from.to_string(),
            to: to.to_string(),
            relation: rel.to_string(),
            properties: HashMap::new(),
        }
    }

    fn boot_node(id: &str) -> ExportedNode {
        ExportedNode {
            id: id.to_string(),
            label: "AgentBoot".to_string(),
            display_name: "Boot".to_string(),
            properties: HashMap::new(),
            event_order: None,
        }
    }

    fn archive_node(id: &str, archive_path: &str) -> ExportedNode {
        let mut props = HashMap::new();
        props.insert(
            a2a::ARCHIVE_PATH.to_string(),
            serde_json::Value::String(archive_path.to_string()),
        );
        ExportedNode {
            id: id.to_string(),
            label: "AgentArchive".to_string(),
            display_name: format!("Archive {archive_path}"),
            properties: props,
            event_order: None,
        }
    }

    /// MessageProcessing activity: links messages/llm/tools to the agent.
    fn mp_node(id: &str) -> ExportedNode {
        ExportedNode {
            id: id.to_string(),
            label: "A2AMessageProcessing".to_string(),
            display_name: "MessageProcessing".to_string(),
            properties: HashMap::new(),
            event_order: None,
        }
    }

    /// Edges for full agent chain: AgentRuntimeInstance → AgentBoot → AgentArchive.
    fn agent_chain_edges(agent_id: &str, boot_id: &str, archive_id: &str) -> Vec<ExportedEdge> {
        vec![
            edge(agent_id, EDGE_WAS_SPAWNED_BY, boot_id),
            edge(boot_id, semantic_labels::WAS_BOOTSTRAPPED_BY, archive_id),
        ]
    }

    fn graph(nodes: Vec<ExportedNode>, edges: Vec<ExportedEdge>) -> ExportedGraph {
        ExportedGraph {
            nodes,
            edges,
            scope: ExportScope::Full,
        }
    }

    #[test]
    fn empty_graph_produces_valid_header() {
        let g = graph(vec![], vec![]);
        let output = render_sequence_diagram(&g);
        assert!(output.starts_with("sequenceDiagram"));
        assert!(output.contains("autonumber"));
        // No fallbacks: empty graph has no agents, so no agent participant.
        assert!(!output.contains("participant Agent"));
    }

    #[test]
    fn message_without_complete_agent_chain_is_not_rendered() {
        // Agent without archive_path (e.g. from old provenance) cannot be attributed.
        let mut agent = agent_node("a1", "clickup_agent");
        agent.properties.remove(a2a::ARCHIVE_PATH);
        let g = graph(
            vec![
                agent,
                mp_node("mp1"),
                msg_node("m1", "user", "hello", Some(1)),
            ],
            vec![
                edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
                edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
            ],
        );
        let output = render_sequence_diagram(&g);
        assert!(
            !output.contains("User->>"),
            "strict mode must not emit message arrows when agent lacks archive_path: {output}"
        );
    }

    #[test]
    fn message_processing_without_executing_agent_is_not_rendered() {
        let mut edges = vec![edge("mp1", EDGE_WAS_RECEIVED_BY, "m1")];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                msg_node("m1", "user", "hello", Some(1)),
                agent_node("a1", "clickup_agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "clickup_agent"),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            !output.contains("User->>clickup_agent"),
            "strict mode must not emit message arrows when activity lacks executing agent: {output}"
        );
    }

    #[test]
    fn single_user_message_cycle() {
        let mut edges = vec![
            edge("tc1", "WAS_USED_BY", "args1"),
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("m2", EDGE_WAS_EMITTED_BY, "mp1"),
            edge("mp1", EDGE_WAS_INVOKED_BY, "llm1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                msg_node("m1", "user", "create a task", Some(1)),
                agent_node("a1", "clickup_agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "clickup_agent"),
                llm_node("llm1", "deepseek/v3", 5000, Some(3)),
                tool_node("tc1", "support/clickupNavigate", 150, Some(4)),
                args_node("args1", r#"{"action":"ListTeams"}"#),
                msg_node("m2", "assistant", "Done! Created the task.", Some(6)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);

        // Participants declared.
        assert!(output.contains("actor User"), "should declare User actor");
        assert!(
            output.contains("participant clickup_agent"),
            "should declare agent participant: {output}"
        );
        assert!(
            output.contains("participant clickupNavigate"),
            "should declare tool participant (prefix stripped): {output}"
        );

        // Message arrows.
        assert!(
            output.contains("User->>+clickup_agent: create a task"),
            "user message arrow (activates agent): {output}"
        );
        assert!(
            output.contains("clickup_agent->>-User: Done! Created the task."),
            "assistant response arrow (deactivates agent): {output}"
        );

        // LLM as participant: Agent->>LLM and LLM-->>Agent arrows.
        assert!(
            output.contains("clickup_agent->>+LLM_deepseek_v3: call"),
            "LLM request arrow: {output}"
        );
        assert!(
            output.contains("LLM_deepseek_v3-->>-clickup_agent: 5000ms ✓"),
            "LLM response arrow: {output}"
        );

        // Tool call arrows (activation on request, deactivation on response).
        assert!(
            output.contains("clickup_agent->>+clickupNavigate: action=ListTeams"),
            "tool call arrow with args: {output}"
        );
        assert!(
            output.contains("clickupNavigate-->>-clickup_agent: 150ms ✓"),
            "tool response arrow: {output}"
        );
    }

    /// Delegation: user message to delegate and assistant reply must show coordinator↔worker,
    /// not User↔worker. Regression for coordinator→specialist misattribution.
    /// Three user messages (hi x3) create three tasks. Each task must get its own section
    /// note with humanized status. Regression for aggregation where newly opened tasks
    /// collapsed into one section.
    #[test]
    fn multi_task_shows_separate_task_notes_per_task() {
        let mut nodes = vec![
            agent_node("a1", "demo"),
            boot_node("boot1"),
            archive_node("arch1", "demo"),
            mp_node("mp1"),
            mp_node("mp2"),
            mp_node("mp3"),
            msg_node("m1", "user", "hi", Some(1)),
            msg_node("m2", "assistant", "Wotcha!", Some(4)),
            msg_node("m3", "user", "hi", Some(5)),
            msg_node("m4", "assistant", "Hello again", Some(8)),
            msg_node("m5", "user", "hi", Some(9)),
            msg_node("m6", "assistant", "Hi there", Some(12)),
        ];
        // TaskExecution nodes (ids encode task_id for enrich)
        nodes.push(ExportedNode {
            id: "task_execution_task-1".to_string(),
            label: "A2ATaskExecution".to_string(),
            display_name: "TaskExec 1".to_string(),
            properties: HashMap::new(),
            event_order: Some(0),
        });
        nodes.push(ExportedNode {
            id: "task_execution_task-2".to_string(),
            label: "A2ATaskExecution".to_string(),
            display_name: "TaskExec 2".to_string(),
            properties: HashMap::new(),
            event_order: Some(4),
        });
        nodes.push(ExportedNode {
            id: "task_execution_task-3".to_string(),
            label: "A2ATaskExecution".to_string(),
            display_name: "TaskExec 3".to_string(),
            properties: HashMap::new(),
            event_order: Some(8),
        });
        // TaskState nodes for status labels
        nodes.push(ExportedNode {
            id: "task_state:task-1:TASK_STATE_COMPLETED".to_string(),
            label: "A2ATaskState".to_string(),
            display_name: "Completed".to_string(),
            properties: {
                let mut p = HashMap::new();
                p.insert(
                    a2a::TASK_STATE.to_string(),
                    serde_json::json!("TASK_STATE_COMPLETED"),
                );
                p
            },
            event_order: Some(3),
        });
        nodes.push(ExportedNode {
            id: "task_state:task-2:TASK_STATE_INPUT_REQUIRED".to_string(),
            label: "A2ATaskState".to_string(),
            display_name: "Input required".to_string(),
            properties: {
                let mut p = HashMap::new();
                p.insert(
                    a2a::TASK_STATE.to_string(),
                    serde_json::json!("TASK_STATE_INPUT_REQUIRED"),
                );
                p
            },
            event_order: Some(7),
        });
        nodes.push(ExportedNode {
            id: "task_state:task-3:TASK_STATE_COMPLETED".to_string(),
            label: "A2ATaskState".to_string(),
            display_name: "Completed".to_string(),
            properties: {
                let mut p = HashMap::new();
                p.insert(
                    a2a::TASK_STATE.to_string(),
                    serde_json::json!("TASK_STATE_COMPLETED"),
                );
                p
            },
            event_order: Some(11),
        });
        let mut edges = vec![
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("m2", EDGE_WAS_EMITTED_BY, "mp1"),
            edge("mp1", EDGE_WAS_INVOKED_BY, "task_execution_task-1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
            edge("mp2", EDGE_WAS_RECEIVED_BY, "m3"),
            edge("m4", EDGE_WAS_EMITTED_BY, "mp2"),
            edge("mp2", EDGE_WAS_INVOKED_BY, "task_execution_task-2"),
            edge("mp2", EDGE_WAS_EXECUTED_BY, "a1"),
            edge("mp3", EDGE_WAS_RECEIVED_BY, "m5"),
            edge("m6", EDGE_WAS_EMITTED_BY, "mp3"),
            edge("mp3", EDGE_WAS_INVOKED_BY, "task_execution_task-3"),
            edge("mp3", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let mut g = graph(nodes, edges);
        enrich::enrich_derived_properties(&mut g);
        let output = render_sequence_diagram(&g);
        // Must have 3 task section notes (one per task)
        let note_count = output.matches("Note over").count();
        assert!(
            note_count >= 3,
            "expected at least 3 task section notes, got {note_count}; output:\n{output}"
        );
        // Status labels must appear (humanized)
        assert!(
            output.contains("Completed"),
            "task status 'Completed' should appear: {output}"
        );
        assert!(
            output.contains("Input Required") || output.contains("Input required"),
            "task status 'Input required' should appear: {output}"
        );
    }

    #[test]
    fn delegation_shows_coordinator_as_sender_and_recipient_not_user() {
        let persona = "conversational_persona_demo_1_0_0";
        let worker = "claude_session_demo_1_0_0";
        let mut edges = vec![
            // Persona receives initial user message
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a_persona"),
            // Persona calls internal_a2a (delegation)
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("tc1", semantic_labels::WAS_DELEGATED_TO, "dt1"),
            edge("tc1", "WAS_USED_BY", "args1"),
            // Worker receives delegated message
            edge("mp2", EDGE_WAS_RECEIVED_BY, "m2"),
            edge("mp2", EDGE_WAS_EXECUTED_BY, "a_worker"),
            // Worker emits reply
            edge("m3", EDGE_WAS_EMITTED_BY, "mp2"),
        ];
        edges.extend(agent_chain_edges("a_persona", "boot_p", "arch_p"));
        edges.extend(agent_chain_edges("a_worker", "boot_w", "arch_w"));
        let g = graph(
            vec![
                agent_node("a_persona", persona),
                agent_node("a_worker", worker),
                boot_node("boot_p"),
                boot_node("boot_w"),
                archive_node("arch_p", persona),
                archive_node("arch_w", worker),
                mp_node("mp1"),
                mp_node("mp2"),
                msg_node("m1", "user", "make me a bash script", Some(1)),
                msg_node(
                    "m2",
                    "user",
                    r#"{"objective":"Create bash script"}"#,
                    Some(3),
                ),
                msg_node("m3", "assistant", "--- Plan --- Requirements: ...", Some(5)),
                tool_node("tc1", "system/internal_a2a", 450, Some(2)),
                args_node("args1", r#"{"target":"claude_session_demo"}"#),
                delegation_target_node("dt1", worker),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            !output.contains("internal_a2a"),
            "A2A mediator tools (internal_a2a) must be omitted from diagram; got:\n{output}"
        );
        assert!(
            output.contains(&format!("{persona}->>+{worker}")),
            "delegated user message must show coordinator->>worker, not User->>worker; got:\n{output}"
        );
        assert!(
            output.contains(&format!("{worker}->>-{persona}")),
            "delegate reply must show worker->>coordinator, not worker->>User; got:\n{output}"
        );
    }

    #[test]
    fn consecutive_agent_responses_deactivate_only_once() {
        let mut edges = vec![
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("m2", EDGE_WAS_EMITTED_BY, "mp1"),
            edge("m3", EDGE_WAS_EMITTED_BY, "mp1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                msg_node("m1", "user", "hello", Some(1)),
                agent_node("a1", "demo"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "demo"),
                msg_node("m2", "assistant", "first reply", Some(2)),
                msg_node("m3", "assistant", "second reply", Some(3)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("demo->>-User: first reply"),
            "first response should deactivate active participant: {output}"
        );
        assert!(
            output.contains("demo->>User: second reply"),
            "second response should not attempt a second deactivation: {output}"
        );
    }

    #[test]
    fn tool_call_without_args() {
        let mut edges = vec![
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "tony"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "tony"),
                tool_node("tc1", "memory/recall", 200, Some(1)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        // No args edge → should show "call" as the label.
        assert!(
            output.contains("tony->>+memory_recall: call"),
            "tool call without args should say 'call': {output}"
        );
    }

    #[test]
    fn role_normalization_in_sequence() {
        let mut edges = vec![
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                msg_node("m1", "ROLE_USER", "hi there", Some(1)),
                agent_node("a1", "bot"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "bot"),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("User->>+bot: hi there"),
            "ROLE_USER should normalize to User arrow (activates agent): {output}"
        );
    }

    #[test]
    fn multiple_tools_get_separate_participants() {
        let mut edges = vec![
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc2"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "agent"),
                tool_node("tc1", "support/clickupNavigate", 100, Some(1)),
                tool_node("tc2", "support/clickupMutate", 200, Some(2)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("participant clickupNavigate"),
            "first tool: {output}"
        );
        assert!(
            output.contains("participant clickupMutate"),
            "second tool: {output}"
        );
    }

    #[test]
    fn dedup_same_tool_across_multiple_calls() {
        let mut edges = vec![
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc2"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "agent"),
                tool_node("tc1", "support/clickupNavigate", 100, Some(1)),
                tool_node("tc2", "support/clickupNavigate", 150, Some(2)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        // Should only declare participant once.
        let count = output.matches("participant clickupNavigate").count();
        assert_eq!(count, 1, "same tool should be declared only once: {output}");
    }

    #[test]
    fn llm_failure_shows_cross() {
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String("gpt-4".to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(3000));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Failed"),
        );
        let node = ExportedNode {
            id: "llm1".to_string(),
            label: "LlmCall".to_string(),
            display_name: "🤖 LLM gpt-4 3000ms ❌".to_string(),
            properties: props,
            event_order: Some(2),
        };
        let mut edges = vec![
            edge("mp1", EDGE_WAS_INVOKED_BY, "llm1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "bot"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "bot"),
                node,
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("LLM_gpt_4--xbot: 3000ms ✗"),
            "failed LLM should show cross arrow: {output}"
        );
    }

    #[test]
    fn escapes_special_characters() {
        assert_eq!(escape_sequence_text(r#"say "hello""#), "say 'hello'");
        assert_eq!(escape_sequence_text("a<b>c"), "a‹b›c");
        assert_eq!(escape_sequence_text("line1\nline2"), "line1 line2");
    }

    #[test]
    fn role_agent_renders_as_response_arrow() {
        let mut edges = vec![
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("m2", EDGE_WAS_EMITTED_BY, "mp1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                msg_node("m1", "user", "create a task", Some(1)),
                agent_node("a1", "clickup_agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "clickup_agent"),
                msg_node("m2", "ROLE_AGENT", "Done! Created task Test11.", Some(5)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("clickup_agent->>-User: Done! Created task Test11."),
            "ROLE_AGENT should render as agent response arrow (deactivates agent): {output}"
        );
    }

    #[test]
    fn sanitize_participant_replaces_special() {
        assert_eq!(sanitize_participant("clickup-agent"), "clickup_agent");
        assert_eq!(sanitize_participant("memory/tony"), "memory_tony");
        assert_eq!(sanitize_participant("simple"), "simple");
    }

    /// Full conversation flow: user message → LLM reasoning → tool call →
    /// agent response message. Verifies temporal ordering via `event_order`
    /// and correct arrow directions for both user and agent messages.
    #[test]
    fn full_conversation_with_agent_response_ordered_by_timestamp() {
        let mut edges = vec![
            edge("tc1", "WAS_USED_BY", "args1"),
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("m2", EDGE_WAS_EMITTED_BY, "mp1"),
            edge("mp1", EDGE_WAS_INVOKED_BY, "llm1"),
            edge("mp1", EDGE_WAS_INVOKED_BY, "llm2"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "clickup_agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "clickup_agent"),
                msg_node("m1", "ROLE_USER", "please create task Test11", Some(1)),
                llm_node("llm1", "deepseek/v3", 4500, Some(3)),
                tool_node("tc1", "support/clickupMutate", 320, Some(5)),
                args_node("args1", r#"{"action":"CreateTask","name":"Test11"}"#),
                llm_node("llm2", "deepseek/v3", 2100, Some(7)),
                msg_node("m2", "ROLE_AGENT", "Done! I created task Test11.", Some(9)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);

        // ── Participants ─────────────────────────────────────────────
        assert!(
            output.contains("actor User"),
            "User actor should be declared: {output}"
        );
        assert!(
            output.contains("participant clickup_agent"),
            "agent participant declared: {output}"
        );
        assert!(
            output.contains("participant clickupMutate"),
            "tool participant declared: {output}"
        );

        // ── Arrow directions ─────────────────────────────────────────
        assert!(
            output.contains("User->>+clickup_agent: please create task Test11"),
            "user message should be User->>Agent (activates agent): {output}"
        );
        assert!(
            output.contains("clickup_agent->>-User: Done! I created task Test11."),
            "agent response should be Agent->>User (deactivates agent): {output}"
        );

        // ── Temporal ordering: arrows appear in order ────────────────
        let user_msg_pos = output
            .find("User->>+clickup_agent: please create task")
            .expect("user message arrow");
        let llm1_pos = output
            .find("clickup_agent->>+LLM_deepseek_v3:")
            .expect("first LLM request arrow");
        let tool_pos = output
            .find("clickup_agent->>+clickupMutate:")
            .expect("tool call arrow");
        let llm2_pos = output
            .find("LLM_deepseek_v3-->>-clickup_agent: 2100ms")
            .expect("second LLM response arrow");
        let agent_response_pos = output
            .find("clickup_agent->>-User: Done! I created task Test11.")
            .expect("agent response arrow");

        assert!(
            user_msg_pos < llm1_pos,
            "user message ({user_msg_pos}) should appear before first LLM ({llm1_pos})"
        );
        assert!(
            llm1_pos < tool_pos,
            "first LLM ({llm1_pos}) should appear before tool call ({tool_pos})"
        );
        assert!(
            tool_pos < llm2_pos,
            "tool call ({tool_pos}) should appear before second LLM ({llm2_pos})"
        );
        assert!(
            llm2_pos < agent_response_pos,
            "second LLM ({llm2_pos}) should appear before agent response ({agent_response_pos})"
        );
    }

    /// Infrastructure agents (runner, client) should not appear as
    /// sequence diagram participants — they are control-plane, not
    /// conversation actors.
    #[test]
    fn infrastructure_agents_excluded_from_participants() {
        // Only clickup_agent has full chain (boot + archive); runner/client do not.
        let mut edges = vec![
            edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "clickup_agent"),
                agent_node("a2", "runner"),
                agent_node("a3", "client"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "clickup_agent"),
                msg_node("m1", "user", "hello", Some(1)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("participant clickup_agent"),
            "real agent should be a participant: {output}"
        );
        assert!(
            !output.contains("participant runner"),
            "runner should NOT be a participant: {output}"
        );
        assert!(
            !output.contains("participant client"),
            "client should NOT be a participant: {output}"
        );
    }

    /// When only an agent response message exists (no explicit user message
    /// in the graph), the User actor should still be declared because a
    /// response implies a user.
    #[test]
    fn agent_response_alone_implies_user_participant() {
        let mut edges = vec![
            edge("m1", EDGE_WAS_EMITTED_BY, "mp1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "tony"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "tony"),
                msg_node("m1", "ROLE_AGENT", "Here is the answer.", Some(5)),
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("actor User"),
            "agent response should imply User actor exists: {output}"
        );
        assert!(
            output.contains("tony->>User: Here is the answer."),
            "agent response arrow (strict sender; no prior activation so no deactivate): {output}"
        );
    }

    /// A failed tool call with an error message in metadata should render
    /// the error preview on the response arrow alongside the ✗ marker.
    #[test]
    fn tool_call_failure_shows_error_detail() {
        let mut props = HashMap::new();
        props.insert(
            a2a::TOOL_NAME.to_string(),
            serde_json::Value::String("support/clickupTasks".to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(569));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Failed"),
        );
        props.insert(
            a2a::METADATA.to_string(),
            serde_json::json!({"error": "list_id is required", "phase": "send"}),
        );
        let node = ExportedNode {
            id: "tc1".to_string(),
            label: "ToolCall".to_string(),
            display_name: "🔧 clickupTasks".to_string(),
            properties: props,
            event_order: Some(2),
        };
        let mut edges = vec![
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "clickup_agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "clickup_agent"),
                node,
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("569ms ✗ list_id is required"),
            "failed tool call should show error detail: {output}"
        );
        assert!(
            output.contains("clickupTasks--xclickup_agent"),
            "failed tool return must use Mermaid cross-headed arrow (--x): {output}"
        );
        assert!(
            output.contains("Note over clickupTasks,clickup_agent: ✗ \"list_id is required\""),
            "failed tool call should have aligned note: {output}"
        );
    }

    /// A failed tool call without an error message in metadata should
    /// still show ✗ but no error text.
    #[test]
    fn tool_call_failure_without_error_detail() {
        let mut props = HashMap::new();
        props.insert(
            a2a::TOOL_NAME.to_string(),
            serde_json::Value::String("support/clickupTasks".to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(273));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Failed"),
        );
        let node = ExportedNode {
            id: "tc1".to_string(),
            label: "ToolCall".to_string(),
            display_name: "🔧 clickupTasks".to_string(),
            properties: props,
            event_order: Some(2),
        };
        let mut edges = vec![
            edge("mp1", EDGE_WAS_EXECUTED_BY, "tc1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "clickup_agent"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "clickup_agent"),
                node,
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("273ms ✗"),
            "failed tool call without error should still show cross: {output}"
        );
        // Should not contain any dangling text after ✗ other than the newline.
        let line = output.lines().find(|l| l.contains("273ms ✗")).unwrap();
        assert!(
            line.trim().ends_with("273ms ✗"),
            "no extra text after ✗ when no error: {line}"
        );
    }

    /// A failed LLM call with an error in metadata should show the error
    /// preview in the Note.
    #[test]
    fn llm_failure_shows_error_detail() {
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String("gpt-4".to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(3000));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Failed"),
        );
        props.insert(
            a2a::METADATA.to_string(),
            serde_json::json!({"error": "rate limit exceeded"}),
        );
        let node = ExportedNode {
            id: "llm1".to_string(),
            label: "LlmCall".to_string(),
            display_name: "🤖 LLM gpt-4 3000ms ❌".to_string(),
            properties: props,
            event_order: Some(2),
        };
        let mut edges = vec![
            edge("mp1", EDGE_WAS_INVOKED_BY, "llm1"),
            edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        ];
        edges.extend(agent_chain_edges("a1", "boot1", "arch1"));
        let g = graph(
            vec![
                agent_node("a1", "bot"),
                mp_node("mp1"),
                boot_node("boot1"),
                archive_node("arch1", "bot"),
                node,
            ],
            edges,
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("LLM_gpt_4--xbot: 3000ms ✗ rate limit exceeded"),
            "failed LLM should show error detail in response arrow: {output}"
        );
    }
}
