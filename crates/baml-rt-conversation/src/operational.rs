//! Operator-facing operational transcript rows (failures, dispatch, task status).
//! Not projected into BAML `conversation_transcript`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalEventKind {
    DispatchRejected,
    DispatchTransportError,
    DispatchAccepted,
    SourcePollRecorded,
    LlmCallFailed,
    PromptRejected,
    TaskStatusChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalEventSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalEventContent {
    pub kind: OperationalEventKind,
    pub severity: OperationalEventSeverity,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_status: Option<String>,
}

impl OperationalEventContent {
    #[must_use]
    pub fn is_meaningful(&self) -> bool {
        !self.summary.trim().is_empty()
    }
}
