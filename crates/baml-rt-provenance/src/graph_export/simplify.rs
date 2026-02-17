//! Graph simplification pass for human-readable diagrams.
//!
//! [`simplify_graph`] transforms an [`ExportedGraph`] by removing noise nodes
//! that clutter visual output without adding semantic value:
//!
//! 1. **LlmPrompt** nodes — always show just "💬 Prompt" with no useful info.
//! 2. **LlmCall start** nodes — have `model: "unknown"` and no duration/success.
//!    The matching *complete* node carries the real model, duration, and outcome.
//! 3. **ToolCall non-send phases** — FSM tools produce open/send/next/finish
//!    phases. Only the `send` phase carries the actual action payload; the rest
//!    are FSM bookkeeping.
//! 4. **ToolCall start** nodes — within the kept `send` phase, remove the start
//!    event (no duration/success) and keep only the complete event.
//! 5. **Orphaned ToolArgs** — ToolArgs connected to removed ToolCalls are dropped.
//!
//! Edges touching any removed node are also removed. The remaining graph stays
//! connected through TaskExecution and MessageProcessing hub nodes which have
//! edges to both start and complete events.

use std::collections::{HashMap, HashSet};

use super::{ExportedEdge, ExportedGraph, ExportedNode};
use crate::graph_model::GraphNodeLabel;
use crate::vocabulary::a2a;

/// Simplify an [`ExportedGraph`] for human-readable rendering.
///
/// This is a *lossy* transformation — it discards provenance detail in
/// exchange for readability. The full graph remains available via the
/// unsimplified [`ExportedGraph`].
pub fn simplify_graph(graph: &ExportedGraph) -> ExportedGraph {
    let mut remove_ids: HashSet<&str> = HashSet::new();

    // ── 1. Remove all LlmPrompt nodes ──────────────────────────────────
    for node in &graph.nodes {
        if node.label == GraphNodeLabel::LlmPrompt.as_str() {
            remove_ids.insert(&node.id);
        }
    }

    // ── 2. Remove LlmCall "start" nodes (no duration_ms / success) ─────
    for node in &graph.nodes {
        if node.label == GraphNodeLabel::LlmCall.as_str() && !is_complete_event(node) {
            remove_ids.insert(&node.id);
        }
    }

    // ── 3+4. Remove ToolCall nodes that aren't "send complete" ──────────
    for node in &graph.nodes {
        if node.label == GraphNodeLabel::ToolCall.as_str() {
            let phase = super::extract_metadata_field(node.properties.get(a2a::METADATA), "phase");
            let complete = is_complete_event(node);

            let keep = match phase.as_deref() {
                // FSM tool: keep only the send-complete event.
                Some("send") => complete,
                // open, finish, next — always noise.
                Some(_) => false,
                // Non-FSM tool (no phase): keep the complete event.
                None => complete,
            };

            if !keep {
                remove_ids.insert(&node.id);
            }
        }
    }

    // ── 5. Remove orphaned ToolArgs ─────────────────────────────────────
    // Collect IDs of kept ToolCall nodes.
    let kept_tool_ids: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|n| {
            n.label == GraphNodeLabel::ToolCall.as_str() && !remove_ids.contains(n.id.as_str())
        })
        .map(|n| n.id.as_str())
        .collect();

    // Find ToolArgs referenced by kept ToolCalls via WAS_USED_BY edges.
    let kept_args_ids: HashSet<&str> = graph
        .edges
        .iter()
        .filter(|e| e.relation == "WAS_USED_BY" && kept_tool_ids.contains(e.from.as_str()))
        .map(|e| e.to.as_str())
        .collect();

    for node in &graph.nodes {
        if node.label == GraphNodeLabel::ToolArgs.as_str()
            && !kept_args_ids.contains(node.id.as_str())
        {
            remove_ids.insert(&node.id);
        }
    }

    // ── 6. Deduplicate AgentRuntimeInstance nodes by agent_type ────────
    //
    // Multiple boot events (across CLI runs) create separate agent nodes
    // with the same `a2a:agent_type`. The scope filter exempts them from
    // context-based removal, so duplicates may survive. We keep one
    // representative per `agent_type` and redirect edges.
    let agent_redirect = dedup_agents_by_type(graph, &mut remove_ids);

    // ── Build filtered graph ────────────────────────────────────────────
    let nodes: Vec<ExportedNode> = graph
        .nodes
        .iter()
        .filter(|n| !remove_ids.contains(n.id.as_str()))
        .cloned()
        .collect();

    let edges: Vec<ExportedEdge> = graph
        .edges
        .iter()
        .filter(|e| !remove_ids.contains(e.from.as_str()) && !remove_ids.contains(e.to.as_str()))
        .map(|e| {
            let mut e = e.clone();
            if let Some(new_id) = agent_redirect.get(e.from.as_str()) {
                e.from = new_id.to_string();
            }
            if let Some(new_id) = agent_redirect.get(e.to.as_str()) {
                e.to = new_id.to_string();
            }
            e
        })
        .collect();

    // Deduplicate edges that may now be identical after redirect, while
    // preserving the temporal insertion order established by `parse_export_result`.
    let mut seen = HashSet::new();
    let mut deduped_edges = edges;
    deduped_edges.retain(|e| seen.insert((e.from.clone(), e.relation.clone(), e.to.clone())));

    ExportedGraph {
        nodes,
        edges: deduped_edges,
        scope: graph.scope.clone(),
    }
}

/// Deduplicate `AgentRuntimeInstance` nodes that share the same `a2a:agent_type`.
///
/// Returns a redirect map: removed_id → kept_id. Edges touching removed agents
/// should be rewritten to point at the kept representative.
fn dedup_agents_by_type<'a>(
    graph: &'a ExportedGraph,
    remove_ids: &mut HashSet<&'a str>,
) -> HashMap<&'a str, &'a str> {
    let mut by_type: HashMap<&str, Vec<&ExportedNode>> = HashMap::new();

    for node in &graph.nodes {
        if node.label == GraphNodeLabel::AgentRuntimeInstance.as_str()
            && !remove_ids.contains(node.id.as_str())
        {
            let agent_type = node
                .properties
                .get(a2a::AGENT_TYPE)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            by_type.entry(agent_type).or_default().push(node);
        }
    }

    let mut redirect: HashMap<&str, &str> = HashMap::new();
    for nodes in by_type.values() {
        if nodes.len() <= 1 {
            continue;
        }
        // Keep the first node (stable: nodes are sorted by id in ExportedGraph).
        let kept = nodes[0];
        for dup in &nodes[1..] {
            remove_ids.insert(&dup.id);
            redirect.insert(dup.id.as_str(), kept.id.as_str());
        }
    }

    redirect
}

/// Check if a node represents a *complete* event (has duration_ms or success).
///
/// Start events are written before the operation runs and lack these fields.
/// Complete events are written after the operation finishes and carry timing
/// and outcome data.
fn is_complete_event(node: &ExportedNode) -> bool {
    node.properties.contains_key(a2a::DURATION_MS) || node.properties.contains_key(a2a::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_export::{ExportScope, ExportedEdge, ExportedGraph, ExportedNode};
    use std::collections::HashMap;

    fn node_with_props(
        id: &str,
        label: &str,
        display: &str,
        props: Vec<(&str, serde_json::Value)>,
    ) -> ExportedNode {
        ExportedNode {
            id: id.to_string(),
            label: label.to_string(),
            display_name: display.to_string(),
            properties: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            event_order: None,
        }
    }

    fn simple_node(id: &str, label: &str, display: &str) -> ExportedNode {
        node_with_props(id, label, display, vec![])
    }

    fn edge(from: &str, rel: &str, to: &str) -> ExportedEdge {
        ExportedEdge {
            from: from.to_string(),
            to: to.to_string(),
            relation: rel.to_string(),
            properties: HashMap::new(),
        }
    }

    fn graph(nodes: Vec<ExportedNode>, edges: Vec<ExportedEdge>) -> ExportedGraph {
        ExportedGraph {
            nodes,
            edges,
            scope: ExportScope::Full,
        }
    }

    #[test]
    fn removes_llm_prompt_nodes() {
        let g = graph(
            vec![
                simple_node("llm-1", "LlmCall", "🤖 LLM"),
                simple_node("prompt-1", "LlmPrompt", "💬 Prompt"),
            ],
            vec![edge("llm-1", "WAS_USED_BY", "prompt-1")],
        );
        let simplified = simplify_graph(&g);
        assert!(
            simplified.nodes.iter().all(|n| n.label != "LlmPrompt"),
            "LlmPrompt should be removed"
        );
        assert!(
            simplified.edges.is_empty(),
            "edge to prompt should be removed"
        );
    }

    #[test]
    fn removes_llm_start_keeps_complete() {
        let g = graph(
            vec![
                // Start: no duration_ms, model unknown.
                node_with_props(
                    "llm-start",
                    "LlmCall",
                    "🤖 LLM unknown (Chat)",
                    vec![
                        (a2a::MODEL, serde_json::json!("unknown")),
                        (a2a::FUNCTION_NAME, serde_json::json!("Chat")),
                    ],
                ),
                // Complete: has duration_ms and success.
                node_with_props(
                    "llm-complete",
                    "LlmCall",
                    "🤖 LLM deepseek/v3 (Chat) 5000ms ✅",
                    vec![
                        (a2a::MODEL, serde_json::json!("deepseek/v3")),
                        (a2a::FUNCTION_NAME, serde_json::json!("Chat")),
                        (a2a::DURATION_MS, serde_json::json!(5000)),
                        (a2a::SUCCESS, serde_json::json!(true)),
                    ],
                ),
                simple_node("texec-1", "A2ATaskExecution", "⚙️ TaskExec"),
            ],
            vec![
                edge("texec-1", "WAS_INVOKED_BY", "llm-start"),
                edge("texec-1", "WAS_INVOKED_BY", "llm-complete"),
            ],
        );
        let simplified = simplify_graph(&g);

        let llm_nodes: Vec<&ExportedNode> = simplified
            .nodes
            .iter()
            .filter(|n| n.label == "LlmCall")
            .collect();
        assert_eq!(llm_nodes.len(), 1, "only complete LlmCall should remain");
        assert_eq!(llm_nodes[0].id, "llm-complete");

        // Edge to start should be gone, edge to complete should remain.
        assert_eq!(simplified.edges.len(), 1);
        assert_eq!(simplified.edges[0].to, "llm-complete");
    }

    #[test]
    fn collapses_fsm_tool_phases() {
        // Simulate a single tool invocation with 4 phases:
        // open-start, open-complete, send-start, send-complete
        let g = graph(
            vec![
                // open-start (no duration, phase=open)
                node_with_props(
                    "tc-open-start",
                    "ToolCall",
                    "🔧 clickupNavigate (open)",
                    vec![
                        (a2a::TOOL_NAME, serde_json::json!("support/clickupNavigate")),
                        (
                            a2a::METADATA,
                            serde_json::json!({"phase": "open", "correlation_id": "corr-1"}),
                        ),
                    ],
                ),
                // open-complete (has duration, phase=open)
                node_with_props(
                    "tc-open-complete",
                    "ToolCall",
                    "🔧 clickupNavigate (open)",
                    vec![
                        (a2a::TOOL_NAME, serde_json::json!("support/clickupNavigate")),
                        (
                            a2a::METADATA,
                            serde_json::json!({"phase": "open", "correlation_id": "corr-1"}),
                        ),
                        (a2a::DURATION_MS, serde_json::json!(50)),
                        (a2a::SUCCESS, serde_json::json!(true)),
                    ],
                ),
                // send-start (no duration, phase=send)
                node_with_props(
                    "tc-send-start",
                    "ToolCall",
                    "🔧 clickupNavigate (send)",
                    vec![
                        (a2a::TOOL_NAME, serde_json::json!("support/clickupNavigate")),
                        (
                            a2a::METADATA,
                            serde_json::json!({"phase": "send", "correlation_id": "corr-1"}),
                        ),
                    ],
                ),
                // send-complete (has duration, phase=send) — the keeper
                node_with_props(
                    "tc-send-complete",
                    "ToolCall",
                    "🔧 clickupNavigate (send)",
                    vec![
                        (a2a::TOOL_NAME, serde_json::json!("support/clickupNavigate")),
                        (
                            a2a::METADATA,
                            serde_json::json!({"phase": "send", "correlation_id": "corr-1"}),
                        ),
                        (a2a::DURATION_MS, serde_json::json!(150)),
                        (a2a::SUCCESS, serde_json::json!(true)),
                    ],
                ),
                // ToolArgs: empty for open, real for send
                simple_node("args-open-start", "ToolArgs", "📋 Args (empty)"),
                simple_node("args-open-complete", "ToolArgs", "📋 Args (empty)"),
                simple_node("args-send-start", "ToolArgs", "📋 Args action=ListTeams"),
                simple_node("args-send-complete", "ToolArgs", "📋 Args action=ListTeams"),
                simple_node("texec-1", "A2ATaskExecution", "⚙️ TaskExec"),
            ],
            vec![
                edge("texec-1", "WAS_EXECUTED_BY", "tc-open-start"),
                edge("texec-1", "WAS_EXECUTED_BY", "tc-open-complete"),
                edge("texec-1", "WAS_EXECUTED_BY", "tc-send-start"),
                edge("texec-1", "WAS_EXECUTED_BY", "tc-send-complete"),
                edge("tc-open-start", "WAS_USED_BY", "args-open-start"),
                edge("tc-open-complete", "WAS_USED_BY", "args-open-complete"),
                edge("tc-send-start", "WAS_USED_BY", "args-send-start"),
                edge("tc-send-complete", "WAS_USED_BY", "args-send-complete"),
            ],
        );
        let simplified = simplify_graph(&g);

        let tc_nodes: Vec<&ExportedNode> = simplified
            .nodes
            .iter()
            .filter(|n| n.label == "ToolCall")
            .collect();
        assert_eq!(
            tc_nodes.len(),
            1,
            "only send-complete ToolCall should remain"
        );
        assert_eq!(tc_nodes[0].id, "tc-send-complete");

        let args_nodes: Vec<&ExportedNode> = simplified
            .nodes
            .iter()
            .filter(|n| n.label == "ToolArgs")
            .collect();
        assert_eq!(
            args_nodes.len(),
            1,
            "only the ToolArgs linked to send-complete should remain"
        );
        assert_eq!(args_nodes[0].id, "args-send-complete");
    }

    #[test]
    fn non_fsm_tool_keeps_complete() {
        // A tool without FSM phases — just start and complete.
        let g = graph(
            vec![
                node_with_props(
                    "tc-start",
                    "ToolCall",
                    "🔧 memory/tony",
                    vec![(a2a::TOOL_NAME, serde_json::json!("memory/tony"))],
                ),
                node_with_props(
                    "tc-complete",
                    "ToolCall",
                    "🔧 memory/tony",
                    vec![
                        (a2a::TOOL_NAME, serde_json::json!("memory/tony")),
                        (a2a::DURATION_MS, serde_json::json!(200)),
                        (a2a::SUCCESS, serde_json::json!(true)),
                    ],
                ),
                simple_node("args-start", "ToolArgs", "📋 Args query=test"),
                simple_node("args-complete", "ToolArgs", "📋 Args query=test"),
            ],
            vec![
                edge("tc-start", "WAS_USED_BY", "args-start"),
                edge("tc-complete", "WAS_USED_BY", "args-complete"),
            ],
        );
        let simplified = simplify_graph(&g);

        let tc_nodes: Vec<&ExportedNode> = simplified
            .nodes
            .iter()
            .filter(|n| n.label == "ToolCall")
            .collect();
        assert_eq!(tc_nodes.len(), 1);
        assert_eq!(tc_nodes[0].id, "tc-complete");
    }

    #[test]
    fn preserves_messages_and_agents() {
        let g = graph(
            vec![
                simple_node("msg-1", "Message", "📩 user: hello"),
                simple_node("agent-1", "AgentRuntimeInstance", "🖥️ Agent clickup"),
                simple_node("mp-1", "A2AMessageProcessing", "🔄 MsgProc"),
            ],
            vec![
                edge("mp-1", "WAS_RECEIVED_BY", "msg-1"),
                edge("mp-1", "WAS_EXECUTED_BY", "agent-1"),
            ],
        );
        let simplified = simplify_graph(&g);
        assert_eq!(simplified.nodes.len(), 3, "all nodes should be preserved");
        assert_eq!(simplified.edges.len(), 2, "all edges should be preserved");
    }

    #[test]
    fn full_scenario_reduces_node_count() {
        // Simulate a mini version of the real graph: 2 LLM calls (start+complete),
        // 1 FSM tool invocation (4 phases), prompts.
        let g = graph(
            vec![
                // Messages + infrastructure
                simple_node("msg-1", "Message", "📩 user: create task"),
                simple_node("mp-1", "A2AMessageProcessing", "🔄 MsgProc"),
                simple_node("texec-1", "A2ATaskExecution", "⚙️ TaskExec"),
                simple_node("agent-1", "AgentRuntimeInstance", "🖥️ Agent clickup"),
                // LLM pair 1
                simple_node("llm-s1", "LlmCall", "🤖 LLM unknown"),
                node_with_props(
                    "llm-c1",
                    "LlmCall",
                    "🤖 LLM deepseek 5000ms ✅",
                    vec![
                        (a2a::DURATION_MS, serde_json::json!(5000)),
                        (a2a::SUCCESS, serde_json::json!(true)),
                    ],
                ),
                // Prompts
                simple_node("p1", "LlmPrompt", "💬 Prompt"),
                simple_node("p2", "LlmPrompt", "💬 Prompt"),
                // ToolCall FSM (4 phases)
                simple_node("tc-os", "ToolCall", "🔧 open start"),
                node_with_props(
                    "tc-oc",
                    "ToolCall",
                    "🔧 open complete",
                    vec![
                        (a2a::METADATA, serde_json::json!({"phase": "open"})),
                        (a2a::DURATION_MS, serde_json::json!(10)),
                    ],
                ),
                node_with_props(
                    "tc-ss",
                    "ToolCall",
                    "🔧 send start",
                    vec![(a2a::METADATA, serde_json::json!({"phase": "send"}))],
                ),
                node_with_props(
                    "tc-sc",
                    "ToolCall",
                    "🔧 send complete",
                    vec![
                        (a2a::METADATA, serde_json::json!({"phase": "send"})),
                        (a2a::DURATION_MS, serde_json::json!(150)),
                        (a2a::SUCCESS, serde_json::json!(true)),
                    ],
                ),
                // ToolArgs (4 matching)
                simple_node("a-os", "ToolArgs", "📋 Args (empty)"),
                simple_node("a-oc", "ToolArgs", "📋 Args (empty)"),
                simple_node("a-ss", "ToolArgs", "📋 Args action=Create"),
                simple_node("a-sc", "ToolArgs", "📋 Args action=Create"),
            ],
            vec![
                edge("mp-1", "WAS_RECEIVED_BY", "msg-1"),
                edge("texec-1", "WAS_INVOKED_BY", "llm-s1"),
                edge("texec-1", "WAS_INVOKED_BY", "llm-c1"),
                edge("llm-s1", "WAS_USED_BY", "p1"),
                edge("llm-c1", "WAS_USED_BY", "p2"),
                edge("texec-1", "WAS_EXECUTED_BY", "tc-os"),
                edge("texec-1", "WAS_EXECUTED_BY", "tc-oc"),
                edge("texec-1", "WAS_EXECUTED_BY", "tc-ss"),
                edge("texec-1", "WAS_EXECUTED_BY", "tc-sc"),
                edge("tc-os", "WAS_USED_BY", "a-os"),
                edge("tc-oc", "WAS_USED_BY", "a-oc"),
                edge("tc-ss", "WAS_USED_BY", "a-ss"),
                edge("tc-sc", "WAS_USED_BY", "a-sc"),
            ],
        );

        // Before: 16 nodes, 13 edges
        assert_eq!(g.nodes.len(), 16);

        let simplified = simplify_graph(&g);

        // After: msg-1, mp-1, texec-1, agent-1, llm-c1, tc-sc, a-sc = 7 nodes
        assert_eq!(
            simplified.nodes.len(),
            7,
            "should reduce from 16 to 7 nodes: {:?}",
            simplified.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
        );

        // Verify no prompts, no starts, no open phases
        assert!(simplified.nodes.iter().all(|n| n.label != "LlmPrompt"));
        assert!(!simplified.nodes.iter().any(|n| n.id == "llm-s1"));
        assert!(!simplified.nodes.iter().any(|n| n.id == "tc-os"));
        assert!(!simplified.nodes.iter().any(|n| n.id == "tc-oc"));
        assert!(!simplified.nodes.iter().any(|n| n.id == "tc-ss"));
    }
}
