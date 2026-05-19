//! `host.source-records.v1` batch types for GitHub Issues polling.

use baml_rt_core::{
    event_subscription::{EventSourceKey, EventSourceKind},
    host_wire::wire,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubIssuesSourceRecordsSource {
    pub source_kind: String,
    pub source_key: String,
    pub source_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubIssuesProjectContext {
    pub project_key: String,
    #[serde(default)]
    pub repo_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubIssueRecord {
    pub record_kind: String,
    pub key: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Value>,
}

/// Wire batch for GitHub Issues polls (`host.source-records.v1`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubIssuesSourceRecordsBatch {
    pub schema_version: String,
    pub emitted_at_unix: u64,
    pub source: GithubIssuesSourceRecordsSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<GithubIssuesProjectContext>,
    pub records: Vec<GithubIssueRecord>,
}

#[derive(Debug, Clone)]
pub struct GithubIssueRecordInput {
    pub key: String,
    pub title: String,
    pub description: String,
    pub priority: String,
    pub sources: Vec<Value>,
}

pub fn batch_from_issue_records(
    source_key: &str,
    source_label: &str,
    project: Option<GithubIssuesProjectContext>,
    records: &[GithubIssueRecordInput],
    emitted_at_unix: u64,
) -> GithubIssuesSourceRecordsBatch {
    let source_kind =
        EventSourceKind::parse("github_issues").expect("github_issues is a valid source kind");
    let _ = EventSourceKey::parse(source_key).expect("caller must pass a valid source_key");

    GithubIssuesSourceRecordsBatch {
        schema_version: wire::HOST_SOURCE_RECORDS_V1.to_string(),
        emitted_at_unix,
        source: GithubIssuesSourceRecordsSource {
            source_kind: source_kind.as_str().to_string(),
            source_key: source_key.to_string(),
            source_label: source_label.to_string(),
        },
        project,
        records: records
            .iter()
            .map(|record| GithubIssueRecord {
                record_kind: "github.issue".to_string(),
                key: record.key.clone(),
                title: record.title.clone(),
                description: record.description.clone(),
                priority: record.priority.clone(),
                sources: record.sources.clone(),
            })
            .collect(),
    }
}

pub fn github_issues_source_records_json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(GithubIssuesSourceRecordsBatch))
        .expect("GithubIssuesSourceRecordsBatch schema serializes to JSON")
}

pub fn github_issues_source_records_sample_payload() -> Value {
    let batch = batch_from_issue_records(
        "github:owner/repo:issues",
        "GitHub Issues",
        Some(GithubIssuesProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: false,
            repo_path: None,
        }),
        &[],
        1_735_720_000,
    );
    serde_json::to_value(&batch).expect("serialize github issues sample batch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_round_trips() {
        let payload = github_issues_source_records_sample_payload();
        let _: GithubIssuesSourceRecordsBatch =
            serde_json::from_value(payload).expect("sample deserializes");
    }

    #[test]
    fn schema_differs_from_clickup_lifecycle_kind() {
        let batch = batch_from_issue_records(
            "github:o/r:issues",
            "issues",
            None,
            &[GithubIssueRecordInput {
                key: "github:owner/repo#1".to_string(),
                title: "Issue".to_string(),
                description: "body".to_string(),
                priority: "medium".to_string(),
                sources: vec![],
            }],
            0,
        );
        assert_eq!(batch.schema_version, wire::HOST_SOURCE_RECORDS_V1);
        assert_eq!(batch.source.source_kind, "github_issues");
        assert_eq!(batch.records[0].record_kind, "github.issue");
        let wire_json = serde_json::to_string(&batch).expect("serialize batch");
        assert!(wire_json.contains("github.issue"));
        assert!(!wire_json.contains("clickup.lifecycle_task"));
    }
}
