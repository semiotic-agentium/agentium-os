//! `host.source-records.v1` batch types for ClickUp lifecycle polling.

use baml_rt_core::{
    event_subscription::{EventSourceKey, EventSourceKind},
    host_wire::wire,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClickupSourceRecordsSource {
    pub source_kind: String,
    pub source_key: String,
    pub source_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClickupProjectContext {
    pub project_key: String,
    #[serde(default)]
    pub repo_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClickupLifecycleTaskRecord {
    pub record_kind: String,
    pub key: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Value>,
}

/// Wire batch for ClickUp lifecycle task polls (`host.source-records.v1`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClickupSourceRecordsBatch {
    pub schema_version: String,
    pub emitted_at_unix: u64,
    pub source: ClickupSourceRecordsSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ClickupProjectContext>,
    pub records: Vec<ClickupLifecycleTaskRecord>,
}

/// Input for one lifecycle task row when building a batch from a host poll.
#[derive(Debug, Clone)]
pub struct ClickupLifecycleTaskInput {
    pub key: String,
    pub title: String,
    pub description: String,
    pub priority: String,
    pub sources: Vec<Value>,
}

/// Build a typed ClickUp source-records batch for host publish.
pub fn batch_from_lifecycle_tasks(
    source_key: &str,
    source_label: &str,
    project: Option<ClickupProjectContext>,
    tasks: &[ClickupLifecycleTaskInput],
    emitted_at_unix: u64,
) -> ClickupSourceRecordsBatch {
    let source_kind = EventSourceKind::parse("clickup").expect("clickup is a valid source kind");
    let _ = EventSourceKey::parse(source_key).expect("caller must pass a valid source_key");

    ClickupSourceRecordsBatch {
        schema_version: wire::HOST_SOURCE_RECORDS_V1.to_string(),
        emitted_at_unix,
        source: ClickupSourceRecordsSource {
            source_kind: source_kind.as_str().to_string(),
            source_key: source_key.to_string(),
            source_label: source_label.to_string(),
        },
        project,
        records: tasks
            .iter()
            .map(|task| ClickupLifecycleTaskRecord {
                record_kind: "clickup.lifecycle_task".to_string(),
                key: task.key.clone(),
                title: task.title.clone(),
                description: task.description.clone(),
                priority: task.priority.clone(),
                sources: task.sources.clone(),
            })
            .collect(),
    }
}

/// JSON Schema for one [`ClickupSourceRecordsBatch`] (`messages[]` item).
pub fn clickup_source_records_json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ClickupSourceRecordsBatch))
        .expect("ClickupSourceRecordsBatch schema serializes to JSON")
}

/// Sample batch for operator console / descriptor registration.
pub fn clickup_source_records_sample_payload() -> Value {
    let batch = batch_from_lifecycle_tasks(
        "clickup:list:901325431486",
        "ClickUp list",
        Some(ClickupProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: true,
            repo_path: Some("/repo/agent-platform".to_string()),
        }),
        &[ClickupLifecycleTaskInput {
            key: "clickup-created:task-1:1".to_string(),
            title: "Investigate publish ingress".to_string(),
            description: "Confirm host bus receives source records".to_string(),
            priority: "high".to_string(),
            sources: Vec::new(),
        }],
        1_735_720_000,
    );
    serde_json::to_value(&batch).expect("serialize clickup sample batch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_round_trips() {
        let payload = clickup_source_records_sample_payload();
        let _: ClickupSourceRecordsBatch =
            serde_json::from_value(payload).expect("sample deserializes");
    }

    #[test]
    fn schema_version_matches_core_wire() {
        let batch = batch_from_lifecycle_tasks("clickup:list:1", "list", None, &[], 0);
        assert_eq!(batch.schema_version, wire::HOST_SOURCE_RECORDS_V1);
    }
}
