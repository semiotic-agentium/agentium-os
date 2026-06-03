// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed rows for **agent-visible** conversation: messages, tool call/result, and session FSM
//! steps. Producers (graph readers) construct these; projection renders them for BAML/HTTP/episode.

use std::collections::HashMap;

use baml_rt_core::{
    Citation,
    history_text::{is_history_infrastructure_notice, strip_history_notice_prefix},
    ids::{ActivityAnchorId, ContextId, MessageId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{operational::OperationalEventContent, planning::PlanningEventContent};

/// Re-export: session step op in conversation history (same as core bus wire).
pub type SessionStepOp = baml_rt_core::bus::SessionStepOp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceContextMessage {
    pub message_id: MessageId,
    pub timestamp_ms: u64,
    pub role: String,
    pub content: Vec<String>,
}

/// Tool invocation content — the step args the LLM produced.
/// `args` is the BAML step payload: `{"op":"Send","input":{...}}` forwarded
/// directly to `ToolHandler::describe_invocation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallContent {
    pub tool_name: String,
    pub args: Value,
    pub fsm_phase: ToolSessionPhase,
}

/// Whether the tool result carries meaningful data or a status-only FSM event.
/// `StatusOnly` items are discarded at the conversion boundary; they never reach rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolOutcome {
    Result(Value),
    Error(Value),
    /// FSM bookkeeping (Open/Finish/Abort/sent) — no data to project.
    StatusOnly,
}

/// Tool result content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultContent {
    pub tool_name: String,
    pub fsm_phase: ToolSessionPhase,
    pub outcome: ToolOutcome,
}

/// Step content for a ToolSessionStep provenance event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStepContent {
    pub tool_name: String,
    pub op: SessionStepOp,
    /// `SendDone` only: `tool_result` JSON from the linked `ToolCall` (via `WAS_INFORMED_BY` graph edge),
    /// for ref-table / replay **hydration** only — not rendered as `conversation_history` content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_done_replay_payload: Option<serde_json::Value>,
    /// SearchRead/PageRead only: **raw** rendered archive body lines (from the linked
    /// `SendDone` JSON, same as ref-table `ArchiveEntry::content`), not a pre-formatted
    /// `cat -n` / `grep -n` session read block. Prompt/episode re-apply
    /// `baml_rt_tools::archive_read::format_session_read_body_from_rendered`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_replay_lines: Option<Vec<String>>,
}

/// Typed discriminated content for a conversation history item.
/// Replaces `content: Value` + `source: String` — the source IS the variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConversationItemContent {
    Message {
        text: String,
        /// Validated citation refs (`#N`, `@N`, …) produced by the model in this message.
        /// Populated from CITED graph edges on the Message entity; empty for user messages.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
    },
    ToolCall(ToolCallContent),
    ToolResult(ToolResultContent),
    /// An individual session step — Open/SendDone/SearchRead/PageRead within an in-progress session.
    SessionStep(SessionStepContent),
    /// Host dispatch, LLM/tool failure classification, or task status (operator transcript only).
    Operational(OperationalEventContent),
    /// Intent, plan, and step lifecycle (operator transcript only).
    Planning(PlanningEventContent),
}

/// True when message text is only an FSM opcode label (sometimes mirrored as assistant noise).
fn is_fsm_opcode_message_noise(text: &str) -> bool {
    let core = strip_history_notice_prefix(text).trim();
    matches!(
        core,
        "Open" | "Send" | "Finish" | "Abort" | "SearchRead" | "PageRead"
    )
}

fn session_tool_args_non_empty(args: &Value) -> bool {
    let step = args.get("step").unwrap_or(args);
    if let Some(input) = step.get("input") {
        return !value_is_empty(input);
    }
    !value_is_empty(args)
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(_) | Value::Number(_) => false,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty() || a.iter().all(value_is_empty),
        Value::Object(m) => m.is_empty() || m.values().all(value_is_empty),
    }
}

impl ConversationItemContent {
    /// Whether this item carries meaningful content worth projecting into a prompt.
    /// `StatusOnly` tool results return false.
    pub fn is_meaningful(&self) -> bool {
        match self {
            Self::Message { text, .. } => {
                let t = text.trim();
                !t.is_empty()
                    && !is_history_infrastructure_notice(text)
                    && !is_fsm_opcode_message_noise(text)
            }
            Self::ToolCall(tc) => {
                if !tc.fsm_phase.is_session_phase() {
                    return true;
                }
                matches!(tc.fsm_phase, ToolSessionPhase::Send)
                    && session_tool_args_non_empty(&tc.args)
            }
            Self::ToolResult(tr) => {
                if matches!(tr.outcome, ToolOutcome::StatusOnly) {
                    return false;
                }
                !tr.fsm_phase.is_session_phase()
            }
            Self::SessionStep(ss) => {
                // `Open` is FSM bookkeeping only. `SendDone` is omitted from the transcript but
                // must still flow through projection so replay payloads seed the ref table.
                !matches!(ss.op, SessionStepOp::Open)
            }
            Self::Operational(op) => op.is_meaningful(),
            Self::Planning(plan) => plan.is_meaningful(),
        }
    }
}

/// Who spoke on a **user** transcript row (trust / UI styling). Symmetric with HTTP `user_speaker_kind`
/// and client `userSpeakerKind` / `ChatMessage.speakerKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserSpeakerKind {
    Human,
    Relay,
    Ingress,
}

impl UserSpeakerKind {
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Human => baml_rt_vocabulary::vocabulary::user_speaker_kinds::HUMAN,
            Self::Relay => baml_rt_vocabulary::vocabulary::user_speaker_kinds::RELAY,
            Self::Ingress => baml_rt_vocabulary::vocabulary::user_speaker_kinds::INGRESS,
        }
    }

    pub fn from_wire_str(raw: &str) -> Option<Self> {
        match raw.trim() {
            baml_rt_vocabulary::vocabulary::user_speaker_kinds::HUMAN => Some(Self::Human),
            baml_rt_vocabulary::vocabulary::user_speaker_kinds::RELAY => Some(Self::Relay),
            baml_rt_vocabulary::vocabulary::user_speaker_kinds::INGRESS => Some(Self::Ingress),
            _ => None,
        }
    }
}

/// Classify a user transcript row. Returns `None` when `role` is not a user turn.
#[must_use]
pub fn classify_user_speaker_kind(
    context_id: &ContextId,
    activity_anchor: &ActivityAnchorId,
    metadata: Option<&HashMap<String, String>>,
    role: &str,
) -> Option<UserSpeakerKind> {
    let r = role.trim();
    if !(r.eq_ignore_ascii_case("ROLE_USER") || r.eq_ignore_ascii_case("user")) {
        return None;
    }
    let anchor = activity_anchor.as_str();
    if let Some(meta) = metadata
        && let Some(raw) = meta.get("user_speaker_kind").map(String::as_str)
        && let Some(kind) = UserSpeakerKind::from_wire_str(raw)
    {
        return Some(kind);
    }
    if anchor.starts_with("ingress-poll-user:") || anchor.starts_with("ingress-unit-user:") {
        return Some(UserSpeakerKind::Ingress);
    }
    if metadata
        .and_then(|m| m.get("kind"))
        .is_some_and(|k| k == "agent-to-agent")
    {
        return Some(UserSpeakerKind::Relay);
    }
    if context_id.as_str().starts_with("a2a:") {
        return Some(UserSpeakerKind::Relay);
    }
    Some(UserSpeakerKind::Human)
}

/// Maps a graph Message `a2a_role` into the `role` field on projected history rows (rendered into `conversation_transcript`).
///
/// Canonical chat labels: **`user`**, **`assistant`**. (Graph may store `ROLE_USER` / `ROLE_AGENT`.)
/// Tool/session rows use **`tool`**; explicit read bodies may use **`read`** (see `prompt_projection`).
#[must_use]
pub fn conversation_history_role_for_message(a2a_role: &str) -> String {
    let r = a2a_role.trim();
    if r.is_empty() {
        return String::new();
    }
    if r.eq_ignore_ascii_case("ROLE_USER") || r.eq_ignore_ascii_case("user") {
        return "user".to_string();
    }
    if r.eq_ignore_ascii_case("ROLE_AGENT")
        || r.eq_ignore_ascii_case("assistant")
        || r.eq_ignore_ascii_case("agent")
    {
        return "assistant".to_string();
    }
    if r.eq_ignore_ascii_case("ROLE_HOST") || r.eq_ignore_ascii_case("host") {
        return "host".to_string();
    }
    a2a_role.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceConversationContextItem {
    pub timestamp_ms: u64,
    /// Correlates this history line with graph `a2a_activity_anchor` / provenance emission ([`ActivityAnchorId`]).
    pub activity_anchor: ActivityAnchorId,
    /// `user` / `assistant` for chat turns; `tool` for host tool calls and session FSM steps; `read` for inlined read bodies.
    pub role: String,
    pub content: ConversationItemContent,
    /// Present only for `role == "user"` transcript rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_speaker_kind: Option<UserSpeakerKind>,
}

impl ProvenanceConversationContextItem {
    /// Returns a string label for the content variant — used in tests and diagnostics.
    pub fn source_name(&self) -> &'static str {
        match &self.content {
            ConversationItemContent::Message { .. } => "message",
            ConversationItemContent::ToolCall(_) => "tool_call",
            ConversationItemContent::ToolResult(_) => "tool_result",
            ConversationItemContent::SessionStep(_) => "session_step",
            ConversationItemContent::Operational(_) => "operational_event",
            ConversationItemContent::Planning(_) => "planning_event",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSessionPhase {
    /// Non-session tool invocation.
    Execute,
    /// FSM phase: session opened.
    Open,
    /// FSM phase: input sent to session; result archived.
    Send,
    /// FSM phase: archived result fetched by archive ref.
    Read,
    /// FSM phase: session continued (legacy analytics label; treat like Send for session semantics).
    Next,
    /// FSM phase: session closed gracefully.
    Finish,
    /// FSM phase: session closed with error.
    Abort,
    Unknown(String),
}

impl ToolSessionPhase {
    /// True for any FSM session phase (Open/Send/Read/Next/Finish/Abort), where `Read` is the
    /// analytics bucket for archive inspection metadata (`search_read` / `page_read`).
    /// These tool calls are represented in history by `SessionStep` events — the
    /// raw ToolCall/ToolResult entries are suppressed to enforce the universal Read interface.
    pub fn is_session_phase(&self) -> bool {
        !matches!(self, Self::Execute | Self::Unknown(_))
    }

    pub fn from_metadata(metadata: &Value) -> Self {
        let phase = metadata
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match phase {
            "execute" => Self::Execute,
            "open" => Self::Open,
            "send" => Self::Send,
            // Archive paging / search share the same session phase bucket for analytics.
            "read" | "search_read" | "page_read" => Self::Read,
            "next" => Self::Next,
            "finish" => Self::Finish,
            "abort" => Self::Abort,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Execute => "execute".to_string(),
            Self::Open => "open".to_string(),
            Self::Send => "send".to_string(),
            Self::Read => "read".to_string(),
            Self::Next => "next".to_string(),
            Self::Finish => "finish".to_string(),
            Self::Abort => "abort".to_string(),
            Self::Unknown(value) => value.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{ContextId, TaskId, UuidId};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn classify_user_speaker_kind_matrix() {
        let ctx = ContextId::new(1, 2);

        let mut ingress_meta = HashMap::new();
        ingress_meta.insert(
            "user_speaker_kind".to_string(),
            baml_rt_vocabulary::vocabulary::user_speaker_kinds::INGRESS.to_string(),
        );
        assert_eq!(
            classify_user_speaker_kind(
                &ctx,
                &ActivityAnchorId::from("derived-host-ingress-anchor"),
                Some(&ingress_meta),
                "user"
            )
            .expect("ingress metadata"),
            UserSpeakerKind::Ingress
        );

        for anchor in [
            "ingress-poll-user:ctx-1-2:msg-1",
            "ingress-unit-user:ctx-1-2:unit-a",
        ] {
            assert_eq!(
                classify_user_speaker_kind(&ctx, &ActivityAnchorId::from(anchor), None, "user")
                    .expect(anchor),
                UserSpeakerKind::Ingress
            );
        }

        let mut relay_meta = HashMap::new();
        relay_meta.insert("kind".to_string(), "agent-to-agent".to_string());
        assert_eq!(
            classify_user_speaker_kind(
                &ctx,
                &ActivityAnchorId::from_counter(99),
                Some(&relay_meta),
                "ROLE_USER"
            )
            .expect("relay metadata"),
            UserSpeakerKind::Relay
        );

        let caller = ContextId::new(1, 2);
        let child_task = TaskId::for_delegated_child(UuidId::new(Uuid::nil()));
        let child_ctx = ContextId::for_a2a_child(&caller, "pkg", "default", &child_task);
        assert_eq!(
            classify_user_speaker_kind(
                &child_ctx,
                &ActivityAnchorId::from_counter(100),
                None,
                "user"
            )
            .expect("a2a child context"),
            UserSpeakerKind::Relay
        );

        assert_eq!(
            classify_user_speaker_kind(&ctx, &ActivityAnchorId::from_counter(101), None, "user")
                .expect("human default"),
            UserSpeakerKind::Human
        );

        assert!(
            classify_user_speaker_kind(
                &ctx,
                &ActivityAnchorId::from_counter(102),
                None,
                "assistant"
            )
            .is_none()
        );
    }

    #[test]
    fn session_open_tool_call_not_meaningful_for_prompt() {
        use serde_json::json;

        let open_call = ConversationItemContent::ToolCall(ToolCallContent {
            tool_name: "support/notion".into(),
            args: json!({ "step": { "op": "Open", "input": {} } }),
            fsm_phase: ToolSessionPhase::Open,
        });
        assert!(!open_call.is_meaningful());

        let send_call = ConversationItemContent::ToolCall(ToolCallContent {
            tool_name: "support/notion".into(),
            args: json!({
                "step": {
                    "op": "Send",
                    "input": { "operation": "search_pages", "query": "OAuth" }
                }
            }),
            fsm_phase: ToolSessionPhase::Send,
        });
        assert!(send_call.is_meaningful());
    }

    #[test]
    fn session_open_step_and_opcode_messages_not_meaningful() {
        let open_step = ConversationItemContent::SessionStep(SessionStepContent {
            tool_name: "support/notion".into(),
            op: SessionStepOp::Open,
            send_done_replay_payload: None,
            read_replay_lines: None,
        });
        assert!(!open_step.is_meaningful());

        let msg = ConversationItemContent::Message {
            text: "#18 Open".into(),
            citations: vec![],
        };
        assert!(!msg.is_meaningful());
    }
}
