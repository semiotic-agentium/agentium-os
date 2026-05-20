//! Core data model shared across polling and source-record publish.

use std::fmt;

use baml_rt_core::EventSourceKind;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
/// High-level source category for a batch.
pub enum TaskSourceKind {
    /// Slack channel polling source.
    Slack,
    /// ClickUp task list polling source.
    Clickup,
    /// Placeholder variant for planned support; extraction is not implemented yet.
    GithubIssues,
}

impl TaskSourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskSourceKind::Slack => "slack",
            TaskSourceKind::Clickup => "clickup",
            TaskSourceKind::GithubIssues => "github_issues",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskSourceKindParseError {
    #[error("unsupported task source kind for task-daemon dispatch: {raw}")]
    Unsupported { raw: String },
}

impl TryFrom<&EventSourceKind> for TaskSourceKind {
    type Error = TaskSourceKindParseError;

    fn try_from(value: &EventSourceKind) -> Result<Self, Self::Error> {
        match value.as_str() {
            "slack" => Ok(TaskSourceKind::Slack),
            "clickup" => Ok(TaskSourceKind::Clickup),
            "github_issues" => Ok(TaskSourceKind::GithubIssues),
            raw => Err(TaskSourceKindParseError::Unsupported {
                raw: raw.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
/// Confidence/priority tier used across interpretation and tasks.
pub enum TaskConfidence {
    Low,
    Medium,
    High,
}

impl TaskConfidence {
    /// Numeric ordering helper (higher is more important/confident).
    pub fn rank(self) -> u8 {
        match self {
            TaskConfidence::Low => 1,
            TaskConfidence::Medium => 2,
            TaskConfidence::High => 3,
        }
    }
}

impl fmt::Display for TaskConfidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            TaskConfidence::Low => "low",
            TaskConfidence::Medium => "medium",
            TaskConfidence::High => "high",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Evidence pointer back to source material.
pub struct SourceReference {
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Normalized Slack message from a poll window.
pub struct SlackMessage {
    pub channel_name: String,
    pub channel_id: String,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    pub source: SourceReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Project metadata resolved from channel config and CLI overrides.
pub struct ProjectContext {
    pub project_key: String,
    pub repo_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
}

impl Default for ProjectContext {
    fn default() -> Self {
        Self {
            project_key: "unscoped-project".to_string(),
            repo_available: false,
            repo_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Task emitted to downstream systems (for example ClickUp).
pub struct InvestigationTask {
    /// Stable key for deduplication/idempotency.
    pub key: String,
    pub title: String,
    pub description: String,
    pub priority: TaskConfidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

#[cfg(test)]
mod tests {
    use baml_rt_core::EventSourceKind;

    use super::{TaskSourceKind, TaskSourceKindParseError};

    #[test]
    fn task_source_kind_parses_from_event_source_kind() {
        let source_kind = EventSourceKind::parse("Slack").expect("source kind");
        assert_eq!(
            TaskSourceKind::try_from(&source_kind).expect("task source kind"),
            TaskSourceKind::Slack
        );
    }

    #[test]
    fn task_source_kind_rejects_unknown_event_source_kind() {
        let source_kind = EventSourceKind::parse("notion").expect("source kind");
        assert_eq!(
            TaskSourceKind::try_from(&source_kind).expect_err("unknown source must fail"),
            TaskSourceKindParseError::Unsupported {
                raw: "notion".to_string()
            }
        );
    }
}
