// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed rows for **agent-visible** conversation: messages, tool call/result, and session FSM
//! steps. Producers (graph readers) construct these; projection renders them for BAML/HTTP/episode.

use std::collections::HashMap;

use baml_rt_core::{
    Citation,
    ids::{ActivityAnchorId, ContextId, MessageId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}

impl ConversationItemContent {
    /// Whether this item carries meaningful content worth projecting into a prompt.
    /// `StatusOnly` tool results return false.
    pub fn is_meaningful(&self) -> bool {
        match self {
            Self::Message { text, .. } => !text.trim().is_empty(),
            Self::ToolCall(_) => true,
            Self::ToolResult(tr) => !matches!(tr.outcome, ToolOutcome::StatusOnly),
            Self::SessionStep(_) => true,
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
    fn classify_ingress_poll_anchor() {
        let ctx = ContextId::new(1, 2);
        let anchor = ActivityAnchorId::from("ingress-poll-user:ctx-1-2:msg-1");
        let kind = classify_user_speaker_kind(&ctx, &anchor, None, "user").expect("user");
        assert_eq!(kind, UserSpeakerKind::Ingress);
    }

    #[test]
    fn classify_ingress_unit_anchor() {
        let ctx = ContextId::new(1, 2);
        let anchor = ActivityAnchorId::from("ingress-unit-user:ctx-1-2:unit-a");
        let kind = classify_user_speaker_kind(&ctx, &anchor, None, "user").expect("user");
        assert_eq!(kind, UserSpeakerKind::Ingress);
    }

    #[test]
    fn classify_relay_from_metadata() {
        let ctx = ContextId::new(1, 2);
        let anchor = ActivityAnchorId::from_counter(99);
        let mut meta = HashMap::new();
        meta.insert("kind".to_string(), "agent-to-agent".to_string());
        let kind =
            classify_user_speaker_kind(&ctx, &anchor, Some(&meta), "ROLE_USER").expect("user");
        assert_eq!(kind, UserSpeakerKind::Relay);
    }

    #[test]
    fn classify_relay_from_a2a_child_context() {
        let caller = ContextId::new(1, 2);
        let child_task = TaskId::for_delegated_child(UuidId::new(Uuid::nil()));
        let ctx = ContextId::for_a2a_child(&caller, "pkg", "default", &child_task);
        let anchor = ActivityAnchorId::from_counter(100);
        let kind = classify_user_speaker_kind(&ctx, &anchor, None, "user").expect("user");
        assert_eq!(kind, UserSpeakerKind::Relay);
    }

    #[test]
    fn classify_human_default() {
        let ctx = ContextId::new(1, 2);
        let anchor = ActivityAnchorId::from_counter(101);
        let kind = classify_user_speaker_kind(&ctx, &anchor, None, "user").expect("user");
        assert_eq!(kind, UserSpeakerKind::Human);
    }

    #[test]
    fn classify_non_user_returns_none() {
        let ctx = ContextId::new(1, 2);
        let anchor = ActivityAnchorId::from_counter(102);
        assert!(classify_user_speaker_kind(&ctx, &anchor, None, "assistant").is_none());
    }
}
