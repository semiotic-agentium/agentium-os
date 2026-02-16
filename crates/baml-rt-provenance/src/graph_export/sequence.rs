//! Mermaid sequence diagram renderer for [`ExportedGraph`].
//!
//! Produces a Mermaid `sequenceDiagram` that shows the temporal narrative of a
//! provenance graph: user prompts → LLM reasoning → tool calls → responses,
//! ordered chronologically with autonumbering.
//!
//! Unlike the flowchart renderer (`mermaid.rs`) which shows structural
//! relationships, this renderer is inherently temporal: participants line up
//! across the top and messages flow downward in causal order.

use std::collections::HashSet;
use std::fmt::Write;

use super::{ExportedGraph, ExportedNode};
use crate::graph_model::GraphNodeLabel;
use crate::vocabulary::{a2a, agent_types};

/// Maximum character length for content previews on sequence diagram arrows.
const SEQUENCE_CONTENT_PREVIEW_LEN: usize = 50;

/// Maximum character length for tool-args summaries on sequence diagram arrows.
const SEQUENCE_ARGS_SUMMARY_LEN: usize = 40;

/// Render an [`ExportedGraph`] as a Mermaid `sequenceDiagram` string.
///
/// The graph should be simplified (via [`super::simplify::simplify_graph`]) and
/// temporally sorted (nodes ordered by `event_order`) before calling this.
pub fn render_sequence_diagram(graph: &ExportedGraph) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "sequenceDiagram");
    let _ = writeln!(out, "    autonumber");

    // ── 1. Identify participants ────────────────────────────────────────
    let participants = extract_participants(graph);

    if participants.has_user {
        let _ = writeln!(out, "    actor User");
    }
    for agent in &participants.agents {
        let _ = writeln!(out, "    participant {}", sanitize_participant(agent));
    }
    for tool in &participants.tools {
        let _ = writeln!(out, "    participant {}", sanitize_participant(tool));
    }
    let _ = writeln!(out);

    // ── 2. Walk nodes in event_order, emit messages ─────────────────────
    let default_agent = participants
        .agents
        .first()
        .cloned()
        .unwrap_or_else(|| "Agent".to_string());
    let agent_alias = sanitize_participant(&default_agent);

    for node in &graph.nodes {
        emit_node(&mut out, node, graph, &agent_alias);
    }

    out
}

// ── Node → sequence message mapping ─────────────────────────────────────────

/// Emit the appropriate sequence diagram line(s) for a single node.
fn emit_node(out: &mut String, node: &ExportedNode, graph: &ExportedGraph, agent: &str) {
    match GraphNodeLabel::parse(&node.label) {
        Some(GraphNodeLabel::Message) => emit_message(out, node, agent),
        Some(GraphNodeLabel::LlmCall) => emit_llm_call(out, node, agent),
        Some(GraphNodeLabel::ToolCall) => emit_tool_call(out, node, graph, agent),
        // MessageProcessing, TaskExecution, etc. are structural — skip.
        _ => {}
    }
}

/// Emit a `User->>Agent` or `Agent->>User` arrow for a Message node.
fn emit_message(out: &mut String, node: &ExportedNode, agent: &str) {
    let role = node
        .properties
        .get(a2a::ROLE)
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let normalized = normalize_role(role);
    let content = super::extract_content_preview(
        node.properties.get(a2a::CONTENT),
        SEQUENCE_CONTENT_PREVIEW_LEN,
    );
    let escaped = escape_sequence_text(&content);

    match normalized.as_str() {
        "user" => {
            let _ = writeln!(out, "    User->>{agent}: {escaped}");
        }
        // ROLE_AGENT (from JS bridge) and assistant are both agent→user messages.
        "assistant" | "agent" => {
            let _ = writeln!(out, "    {agent}->>User: {escaped}");
        }
        other => {
            let _ = writeln!(out, "    Note over {agent}: {other}: {escaped}");
        }
    }
}

/// Emit a `Note over Agent` for an LLM reasoning step.
fn emit_llm_call(out: &mut String, node: &ExportedNode, agent: &str) {
    let model = prop_str(node, a2a::MODEL).unwrap_or_else(|| "unknown".to_string());
    let mut note = format!("LLM {model}");

    if let Some(dur) = prop_str(node, a2a::DURATION_MS) {
        note.push_str(&format!(" ({dur}ms"));
        if is_success(node.properties.get(a2a::SUCCESS)) {
            note.push_str(" ✓");
        } else if is_failure(node.properties.get(a2a::SUCCESS)) {
            note.push_str(" ✗");
        }
        note.push(')');
    }
    let _ = writeln!(
        out,
        "    Note over {agent}: {}",
        escape_sequence_text(&note)
    );
}

/// Emit `Agent->>Tool` and `Tool-->>Agent` arrows for a ToolCall node.
fn emit_tool_call(out: &mut String, node: &ExportedNode, graph: &ExportedGraph, agent: &str) {
    let tool_raw = node
        .properties
        .get(a2a::TOOL_NAME)
        .and_then(|v| v.as_str())
        .map(strip_tool_prefix)
        .unwrap_or("unknown");
    let tool_participant = sanitize_participant(tool_raw);
    let args_summary = find_tool_args(graph, &node.id);

    // Agent → Tool (request).
    let send_label = if args_summary.is_empty() || args_summary == "(empty)" {
        "call".to_string()
    } else {
        escape_sequence_text(&args_summary)
    };
    let _ = writeln!(out, "    {agent}->>{tool_participant}: {send_label}");

    // Tool → Agent (response).
    let mut response = String::new();
    if let Some(dur) = prop_str(node, a2a::DURATION_MS) {
        response.push_str(&format!("{dur}ms"));
    }
    if is_success(node.properties.get(a2a::SUCCESS)) {
        if !response.is_empty() {
            response.push(' ');
        }
        response.push('✓');
    } else if is_failure(node.properties.get(a2a::SUCCESS)) {
        if !response.is_empty() {
            response.push(' ');
        }
        response.push('✗');
    }
    if response.is_empty() {
        response.push_str("done");
    }
    let _ = writeln!(out, "    {tool_participant}-->>{agent}: {response}");
}

// ── Participant extraction ──────────────────────────────────────────────────

/// Identified participants for the sequence diagram.
struct Participants {
    has_user: bool,
    /// Agent display names (from `a2a:agent_type`).
    agents: Vec<String>,
    /// Tool short names (prefix-stripped, deduplicated).
    tools: Vec<String>,
}

/// Walk the graph once to discover all participants.
fn extract_participants(graph: &ExportedGraph) -> Participants {
    let mut has_user = false;
    let mut agents: Vec<String> = Vec::new();
    let mut seen_agents: HashSet<String> = HashSet::new();
    let mut tools: Vec<String> = Vec::new();
    let mut seen_tools: HashSet<String> = HashSet::new();

    for node in &graph.nodes {
        match GraphNodeLabel::parse(&node.label) {
            Some(GraphNodeLabel::Message) => {
                let role = node
                    .properties
                    .get(a2a::ROLE)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let normalized = normalize_role(role);
                // User messages or agent/assistant responses both imply a
                // User participant exists in the conversation.
                if matches!(normalized.as_str(), "user" | "assistant" | "agent") {
                    has_user = true;
                }
            }
            Some(GraphNodeLabel::AgentRuntimeInstance) => {
                let agent_type = node
                    .properties
                    .get(a2a::AGENT_TYPE)
                    .and_then(|v| v.as_str())
                    .unwrap_or("Agent");
                // Infrastructure agents (runner, client) are not conversation
                // participants — they are control-plane bookkeeping.
                if !is_infrastructure_agent(agent_type)
                    && seen_agents.insert(agent_type.to_string())
                {
                    agents.push(agent_type.to_string());
                }
            }
            Some(GraphNodeLabel::ToolCall) => {
                let tool = node
                    .properties
                    .get(a2a::TOOL_NAME)
                    .and_then(|v| v.as_str())
                    .map(strip_tool_prefix)
                    .unwrap_or("unknown");
                if seen_tools.insert(tool.to_string()) {
                    tools.push(tool.to_string());
                }
            }
            _ => {
                // Other node types don't contribute participants.
            }
        }
    }

    if agents.is_empty() {
        agents.push("Agent".to_string());
    }

    Participants {
        has_user,
        agents,
        tools,
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Normalize a role string: strip `ROLE_` prefix and lowercase.
fn normalize_role(raw: &str) -> String {
    let stripped = raw.strip_prefix("ROLE_").unwrap_or(raw);
    stripped.to_lowercase()
}

/// Extract a string property from a node (handles String, Number, Bool).
fn prop_str(node: &ExportedNode, key: &str) -> Option<String> {
    node.properties.get(key).and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

/// Check if a JSON value represents a success (bool `true` or string `"true"`).
fn is_success(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => false,
    }
}

/// Check if a JSON value represents a failure (bool `false` or string `"false"`).
fn is_failure(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(false)) => true,
        Some(serde_json::Value::String(s)) => s == "false",
        _ => false,
    }
}

/// Strip known tool name prefixes for brevity.
fn strip_tool_prefix(name: &str) -> &str {
    name.strip_prefix("support/").unwrap_or(name)
}

/// Infrastructure agent types that should not appear as sequence diagram
/// participants. These are control-plane agents, not conversation actors.
fn is_infrastructure_agent(agent_type: &str) -> bool {
    matches!(agent_type, agent_types::RUNNER | agent_types::CLIENT)
}

/// Find the ToolArgs summary for a given ToolCall by following `WAS_USED_BY` edges.
fn find_tool_args(graph: &ExportedGraph, tool_call_id: &str) -> String {
    for edge in &graph.edges {
        if edge.from == tool_call_id
            && edge.relation == "WAS_USED_BY"
            && let Some(args_node) = graph.nodes.iter().find(|n| n.id == edge.to)
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

/// Escape text for use in Mermaid sequence diagram arrow labels.
fn escape_sequence_text(s: &str) -> String {
    s.replace('"', "'")
        .replace('<', "‹")
        .replace('>', "›")
        .replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_export::{ExportScope, ExportedEdge, ExportedGraph, ExportedNode};
    use std::collections::HashMap;

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
        props.insert(
            a2a::DURATION_MS.to_string(),
            serde_json::json!(duration_ms),
        );
        props.insert(a2a::SUCCESS.to_string(), serde_json::json!(true));
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
        props.insert(
            a2a::DURATION_MS.to_string(),
            serde_json::json!(duration_ms),
        );
        props.insert(a2a::SUCCESS.to_string(), serde_json::json!(true));
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
    fn empty_graph_produces_valid_header() {
        let g = graph(vec![], vec![]);
        let output = render_sequence_diagram(&g);
        assert!(output.starts_with("sequenceDiagram"));
        assert!(output.contains("autonumber"));
        // Default agent participant when no agents found.
        assert!(output.contains("participant Agent"));
    }

    #[test]
    fn single_user_message_cycle() {
        let g = graph(
            vec![
                msg_node("m1", "user", "create a task", Some(1)),
                agent_node("a1", "clickup_agent"),
                llm_node("llm1", "deepseek/v3", 5000, Some(3)),
                tool_node("tc1", "support/clickupNavigate", 150, Some(4)),
                args_node("args1", r#"{"action":"ListTeams"}"#),
                msg_node("m2", "assistant", "Done! Created the task.", Some(6)),
            ],
            vec![edge("tc1", "WAS_USED_BY", "args1")],
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
            output.contains("User->>clickup_agent: create a task"),
            "user message arrow: {output}"
        );
        assert!(
            output.contains("clickup_agent->>User: Done! Created the task."),
            "assistant response arrow: {output}"
        );

        // LLM note.
        assert!(
            output.contains("Note over clickup_agent: LLM deepseek/v3 (5000ms ✓)"),
            "LLM note: {output}"
        );

        // Tool call arrows.
        assert!(
            output.contains("clickup_agent->>clickupNavigate: action=ListTeams"),
            "tool call arrow with args: {output}"
        );
        assert!(
            output.contains("clickupNavigate-->>clickup_agent: 150ms ✓"),
            "tool response arrow: {output}"
        );
    }

    #[test]
    fn tool_call_without_args() {
        let g = graph(
            vec![
                agent_node("a1", "tony"),
                tool_node("tc1", "memory/recall", 200, Some(1)),
            ],
            vec![],
        );
        let output = render_sequence_diagram(&g);
        // No args edge → should show "call" as the label.
        assert!(
            output.contains("tony->>memory_recall: call"),
            "tool call without args should say 'call': {output}"
        );
    }

    #[test]
    fn role_normalization_in_sequence() {
        let g = graph(
            vec![
                msg_node("m1", "ROLE_USER", "hi there", Some(1)),
                agent_node("a1", "bot"),
            ],
            vec![],
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("User->>bot: hi there"),
            "ROLE_USER should normalize to User arrow: {output}"
        );
    }

    #[test]
    fn multiple_tools_get_separate_participants() {
        let g = graph(
            vec![
                agent_node("a1", "agent"),
                tool_node("tc1", "support/clickupNavigate", 100, Some(1)),
                tool_node("tc2", "support/clickupMutate", 200, Some(2)),
            ],
            vec![],
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
        let g = graph(
            vec![
                agent_node("a1", "agent"),
                tool_node("tc1", "support/clickupNavigate", 100, Some(1)),
                tool_node("tc2", "support/clickupNavigate", 150, Some(2)),
            ],
            vec![],
        );
        let output = render_sequence_diagram(&g);
        // Should only declare participant once.
        let count = output.matches("participant clickupNavigate").count();
        assert_eq!(
            count, 1,
            "same tool should be declared only once: {output}"
        );
    }

    #[test]
    fn llm_failure_shows_cross() {
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String("gpt-4".to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(3000));
        props.insert(a2a::SUCCESS.to_string(), serde_json::json!(false));
        let node = ExportedNode {
            id: "llm1".to_string(),
            label: "LlmCall".to_string(),
            display_name: "🤖 LLM gpt-4 3000ms ❌".to_string(),
            properties: props,
            event_order: Some(2),
        };
        let g = graph(vec![agent_node("a1", "bot"), node], vec![]);
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("LLM gpt-4 (3000ms ✗)"),
            "failed LLM should show cross: {output}"
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
        let g = graph(
            vec![
                msg_node("m1", "user", "create a task", Some(1)),
                agent_node("a1", "clickup_agent"),
                // ROLE_AGENT is what the JS bridge sets on agent response messages.
                msg_node("m2", "ROLE_AGENT", "Done! Created task Test11.", Some(5)),
            ],
            vec![],
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("clickup_agent->>User: Done! Created task Test11."),
            "ROLE_AGENT should render as agent response arrow: {output}"
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
        let g = graph(
            vec![
                // Agent identity (no event_order — sinks to end).
                agent_node("a1", "clickup_agent"),
                // 1. User sends a request.
                msg_node("m1", "ROLE_USER", "please create task Test11", Some(1)),
                // 2. LLM reasoning step.
                llm_node("llm1", "deepseek/v3", 4500, Some(3)),
                // 3. Tool call (clickupMutate).
                tool_node("tc1", "support/clickupMutate", 320, Some(5)),
                args_node("args1", r#"{"action":"CreateTask","name":"Test11"}"#),
                // 4. Second LLM reasoning step (produces final answer).
                llm_node("llm2", "deepseek/v3", 2100, Some(7)),
                // 5. Agent response message (FinalResponse via JS bridge).
                msg_node("m2", "ROLE_AGENT", "Done! I created task Test11.", Some(9)),
            ],
            vec![edge("tc1", "WAS_USED_BY", "args1")],
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
            output.contains("User->>clickup_agent: please create task Test11"),
            "user message should be User->>Agent: {output}"
        );
        assert!(
            output.contains("clickup_agent->>User: Done! I created task Test11."),
            "agent response should be Agent->>User: {output}"
        );

        // ── Temporal ordering: arrows appear in order ────────────────
        let user_msg_pos = output
            .find("User->>clickup_agent: please create task")
            .expect("user message arrow");
        let llm1_pos = output
            .find("Note over clickup_agent: LLM deepseek/v3 (4500ms")
            .expect("first LLM note");
        let tool_pos = output
            .find("clickup_agent->>clickupMutate:")
            .expect("tool call arrow");
        let llm2_pos = output
            .find("Note over clickup_agent: LLM deepseek/v3 (2100ms")
            .expect("second LLM note");
        let agent_response_pos = output
            .find("clickup_agent->>User: Done! I created task Test11.")
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
        let g = graph(
            vec![
                agent_node("a1", "clickup_agent"),
                agent_node("a2", "runner"),
                agent_node("a3", "client"),
                msg_node("m1", "user", "hello", Some(1)),
            ],
            vec![],
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
        let g = graph(
            vec![
                agent_node("a1", "tony"),
                msg_node("m1", "ROLE_AGENT", "Here is the answer.", Some(5)),
            ],
            vec![],
        );
        let output = render_sequence_diagram(&g);
        assert!(
            output.contains("actor User"),
            "agent response should imply User actor exists: {output}"
        );
        assert!(
            output.contains("tony->>User: Here is the answer."),
            "agent response arrow: {output}"
        );
    }
}
