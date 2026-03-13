//! Core data model shared across polling, interpretation, and delivery.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Follow-up action category for non-code next steps.
pub enum FollowUpKind {
    StakeholderQuestion,
    DecisionRequest,
    Clarification,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Condition for when an investigation should be executed.
pub enum InvestigationRunCondition {
    Always,
    RepoAvailable,
    RepoUnavailable,
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
/// Normalized Slack message used by interpretation backends.
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
/// Recorded project decision with rationale and evidence.
pub struct DecisionItem {
    pub decision: String,
    pub rationale: String,
    pub confidence: TaskConfidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Open question extracted from discussion context.
pub struct QuestionItem {
    pub question: String,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Risk statement with impact and mitigation detail.
pub struct RiskItem {
    pub risk: String,
    pub impact: String,
    pub mitigation: String,
    pub confidence: TaskConfidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Follow-up action for stakeholders/decision-making.
pub struct FollowUpItem {
    pub kind: FollowUpKind,
    pub prompt: String,
    pub urgency: TaskConfidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Investigation node that can be handed to an agent workflow.
pub struct InvestigationPrompt {
    pub key: String,
    pub title: String,
    pub goal: String,
    /// Prompt text that can be handed directly to another agent.
    pub prompt: String,
    pub when_to_run: InvestigationRunCondition,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_queries: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    pub confidence: TaskConfidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Clarification node describing information needed before execution.
pub struct ClarificationPrompt {
    pub key: String,
    pub question: String,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
/// Workflow handoff artifact derived from project interpretation.
pub struct WorkflowSeed {
    pub goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub investigation_nodes: Vec<InvestigationPrompt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clarification_nodes: Vec<ClarificationPrompt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_up_nodes: Vec<FollowUpItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
/// Structured interpretation of a discussion window.
pub struct ProjectInterpretation {
    pub executive_summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_objectives: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions_made: Vec<DecisionItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<QuestionItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<RiskItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<FollowUpItem>,
    #[serde(default)]
    pub workflow_seed: WorkflowSeed,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Full payload produced by one daemon poll cycle.
pub struct TaskBatch {
    pub source: TaskSourceKind,
    pub source_label: String,
    pub generated_at_unix: u64,
    pub messages_scanned: usize,
    pub project: ProjectContext,
    pub interpretation: ProjectInterpretation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_tasks: Vec<InvestigationTask>,
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
