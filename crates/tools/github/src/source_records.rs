// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `host.source-records.v1` batch types for GitHub Issues polling (raw events).

use baml_rt_core::{
    event_subscription::{EventSourceKey, EventSourceKind},
    host_wire::wire,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const GITHUB_ISSUE_EVENT_KIND: &str = "github.issue_event";

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

/// One GitHub issue lifecycle diff (opaque to the host).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubIssueEventRecord {
    pub record_kind: String,
    pub key: String,
    pub event: String,
    pub issue_number: u64,
    pub repo: String,
    pub revision: u64,
    pub snapshot: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_snapshot: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubIssuesSourceRecordsBatch {
    pub schema_version: String,
    pub emitted_at_unix: u64,
    pub source: GithubIssuesSourceRecordsSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<GithubIssuesProjectContext>,
    pub records: Vec<GithubIssueEventRecord>,
}

pub fn batch_from_issue_events(
    source_key: &str,
    source_label: &str,
    project: Option<GithubIssuesProjectContext>,
    events: &[GithubIssueEventRecord],
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
        records: events.to_vec(),
    }
}

pub fn github_issues_source_records_json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(GithubIssuesSourceRecordsBatch))
        .expect("GithubIssuesSourceRecordsBatch schema serializes to JSON")
}

pub fn github_issues_source_records_sample_payload() -> Value {
    let batch = batch_from_issue_events(
        "github:owner/repo:issues",
        "GitHub Issues",
        Some(GithubIssuesProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: false,
            repo_path: None,
        }),
        &[GithubIssueEventRecord {
            record_kind: GITHUB_ISSUE_EVENT_KIND.to_string(),
            key: "github:owner/repo#42:1".to_string(),
            event: "opened".to_string(),
            issue_number: 42,
            repo: "owner/repo".to_string(),
            revision: 1,
            snapshot: json!({
                "title": "Sample issue from Event Console",
                "state": "open",
                "body": "Replace repo and issue_number with your repository before publishing."
            }),
            previous_snapshot: None,
        }],
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
    fn issue_event_record_kind() {
        let event = GithubIssueEventRecord {
            record_kind: GITHUB_ISSUE_EVENT_KIND.to_string(),
            key: "github:owner/repo#1:1".to_string(),
            event: "opened".to_string(),
            issue_number: 1,
            repo: "owner/repo".to_string(),
            revision: 1,
            snapshot: json!({ "title": "Issue" }),
            previous_snapshot: None,
        };
        let wire = serde_json::to_string(&event).expect("serialize");
        assert!(wire.contains("github.issue_event"));
        assert!(!wire.contains("\"github.issue\""));
    }
}
