//! Input variant for the Claude API / session.
//!
//! Represents the kinds of input the Claude tool can receive: plain text,
//! text with context, tool/command approvals, patch approvals, and meta-requests.
//!
//! Tool approval is part of the BAML flow (coordinator sees intent: approved/rejected).
//! request_id does not exist in BAML-facing code; the session matches approvals to the
//! pending request internally. When sending to Claude (e.g. permission callback), the
//! session uses a separate internal type that includes request_id.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Single item in context attached to user input (e.g. role + content).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

/// Decision for a patch/review approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecision {
    Approved,
    Rejected,
    #[serde(rename_all = "camelCase")]
    Modified {
        patch: Value,
    },
}

/// Unified user input for the Claude API: text, structured approvals, or meta-requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum UserInput {
    /// Simple text input
    Text { text: String },

    /// Text with additional context
    TextWithContext {
        text: String,
        context: Vec<ContextItem>,
    },

    /// Tool/command approval. BAML-facing: approved/rejected only. request_id does not exist here; session uses internal type when sending to Claude.
    ToolApproval {
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modified_input: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Patch approval (e.g. review flow). Session resolves by `request_id`; BAML sees decision only.
    PatchApproval {
        #[serde(rename = "requestId")]
        request_id: String,
        decision: ReviewDecision,
    },

    /// Request conversation history
    GetHistory,

    /// Request MCP tools list
    ListMcpTools,
}

impl UserInput {
    /// Returns the primary text for display or fallback (e.g. for ToolApproval, a short label).
    pub fn display_text(&self) -> String {
        match self {
            UserInput::Text { text } => text.clone(),
            UserInput::TextWithContext { text, .. } => text.clone(),
            UserInput::ToolApproval {
                approved, reason, ..
            } => {
                let base = if *approved { "Approved" } else { "Rejected" };
                reason
                    .as_ref()
                    .map(|r| format!("{base}: {r}"))
                    .unwrap_or_else(|| base.to_string())
            }
            UserInput::PatchApproval { decision, .. } => match decision {
                ReviewDecision::Approved => "Approved".to_string(),
                ReviewDecision::Rejected => "Rejected".to_string(),
                ReviewDecision::Modified { .. } => "Modified".to_string(),
            },
            UserInput::GetHistory => "GetHistory".to_string(),
            UserInput::ListMcpTools => "ListMcpTools".to_string(),
        }
    }

    /// True if this input is a tool approval (approved or rejected).
    pub fn is_tool_approval(&self) -> bool {
        matches!(self, UserInput::ToolApproval { .. })
    }
}
