// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `host.source-records.v1` batch types for ClickUp lifecycle polling (raw events, no host interpretation).

use baml_rt_core::{
    event_subscription::{EventSourceKey, EventSourceKind},
    host_wire::wire,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CLICKUP_LIFECYCLE_EVENT_KIND: &str = "clickup.lifecycle_event";

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

/// One lifecycle diff emitted by ClickUp polling (opaque to the host; agents interpret).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClickupLifecycleEventRecord {
    pub record_kind: String,
    /// Stable dedup key (`clickup-created:task-id:rev`, etc.).
    pub key: String,
    /// `created` | `terminal` | `removed`
    pub event: String,
    pub task_id: String,
    pub list_id: String,
    pub revision: u64,
    pub snapshot: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_snapshot: Option<Value>,
}

/// Wire batch for ClickUp lifecycle polls (`host.source-records.v1`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClickupSourceRecordsBatch {
    pub schema_version: String,
    pub emitted_at_unix: u64,
    pub source: ClickupSourceRecordsSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ClickupProjectContext>,
    pub records: Vec<ClickupLifecycleEventRecord>,
}

/// Build a typed ClickUp source-records batch for host publish.
pub fn batch_from_lifecycle_events(
    source_key: &str,
    source_label: &str,
    project: Option<ClickupProjectContext>,
    events: &[ClickupLifecycleEventRecord],
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
        records: events.to_vec(),
    }
}

/// JSON Schema for one [`ClickupSourceRecordsBatch`] (`messages[]` item).
pub fn clickup_source_records_json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ClickupSourceRecordsBatch))
        .expect("ClickupSourceRecordsBatch schema serializes to JSON")
}

/// Sample batch for operator console / descriptor registration (one lifecycle task created).
pub fn clickup_source_records_sample_payload() -> Value {
    let batch = batch_from_lifecycle_events(
        "clickup:list:901325431486",
        "ClickUp list",
        Some(ClickupProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: true,
            repo_path: Some("/repo/agent-platform".to_string()),
        }),
        &[ClickupLifecycleEventRecord {
            record_kind: CLICKUP_LIFECYCLE_EVENT_KIND.to_string(),
            key: "clickup-created:task-sample-1:1".to_string(),
            event: "created".to_string(),
            task_id: "task-sample-1".to_string(),
            list_id: "901325431486".to_string(),
            revision: 1,
            snapshot: clickup_task_snapshot_value(
                "task-sample-1",
                "901325431486",
                "Sample task from Event Console",
                "in progress",
                Some("Replace list_id and task_id with your workspace before publishing."),
                Some("https://app.clickup.com/t/task-sample-1"),
                Some("normal"),
            ),
            previous_snapshot: None,
        }],
        1_735_720_000,
    );
    serde_json::to_value(&batch).expect("serialize clickup sample batch")
}

/// Minimal API-shaped task snapshot for lifecycle events.
#[must_use]
pub fn clickup_task_snapshot_value(
    task_id: &str,
    list_id: &str,
    name: &str,
    status: &str,
    description: Option<&str>,
    url: Option<&str>,
    priority: Option<&str>,
) -> Value {
    let mut snap = json!({
        "id": task_id,
        "list_id": list_id,
        "name": name,
        "status": status,
    });
    if let Some(d) = description.filter(|s| !s.is_empty()) {
        snap["description"] = json!(d);
    }
    if let Some(u) = url.filter(|s| !s.is_empty()) {
        snap["url"] = json!(u);
    }
    if let Some(p) = priority.filter(|s| !s.is_empty()) {
        snap["priority"] = json!(p);
    }
    snap
}

#[must_use]
pub fn clickup_previous_snapshot_value(
    list_id: &str,
    name: &str,
    status: &str,
    url: Option<&str>,
) -> Value {
    let mut snap = json!({
        "list_id": list_id,
        "name": name,
        "status": status,
    });
    if let Some(u) = url.filter(|s| !s.is_empty()) {
        snap["url"] = json!(u);
    }
    snap
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
    fn sample_includes_one_lifecycle_record() {
        let payload = clickup_source_records_sample_payload();
        let batch: ClickupSourceRecordsBatch =
            serde_json::from_value(payload).expect("sample deserializes");
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.records[0].record_kind, CLICKUP_LIFECYCLE_EVENT_KIND);
        assert_eq!(batch.records[0].event, "created");
    }

    #[test]
    fn schema_version_matches_core_wire() {
        let batch = batch_from_lifecycle_events("clickup:list:1", "list", None, &[], 0);
        assert_eq!(batch.schema_version, wire::HOST_SOURCE_RECORDS_V1);
    }

    #[test]
    fn lifecycle_event_record_kind_is_wire_constant() {
        let event = ClickupLifecycleEventRecord {
            record_kind: CLICKUP_LIFECYCLE_EVENT_KIND.to_string(),
            key: "clickup-created:t1:1".to_string(),
            event: "created".to_string(),
            task_id: "t1".to_string(),
            list_id: "list-1".to_string(),
            revision: 1,
            snapshot: json!({ "name": "x" }),
            previous_snapshot: None,
        };
        let wire = serde_json::to_string(&event).expect("serialize");
        assert!(wire.contains("clickup.lifecycle_event"));
        assert!(!wire.contains("lifecycle_task"));
    }
}
