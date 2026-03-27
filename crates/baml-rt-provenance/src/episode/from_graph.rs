//! Derive episode timeline metadata from the same [`ExportedGraph`] path as Mermaid/DOT/JSON.
//!
//! The **transcript body** still comes from [`ProvenanceQueryApi::query_conversation_context`]
//! because that path hydrates tool payloads from `provenance_payload` and validates ToolCall–ToolArgs
//! topology — data the raw export nodes do not fully duplicate.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    graph_export::ExportedGraph,
    graph_model::GraphNodeLabel,
    vocabulary::{a2a, prov},
};

#[derive(Clone, Debug)]
pub(crate) struct StatusRow {
    pub timestamp_ms: u64,
    pub event_order: u64,
    pub activity_anchor: String,
    pub old_status: String,
    pub new_status: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ArtifactRow {
    pub timestamp_ms: u64,
    pub event_order: u64,
    pub activity_anchor: String,
    pub name: String,
    pub media_type: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct EpisodeTaskGraphMeta {
    pub terminal: Option<(String, u64)>,
    pub task_start_ms: Option<u64>,
    pub status_rows: Vec<StatusRow>,
    pub artifact_rows: Vec<ArtifactRow>,
    pub intent_citations: HashMap<String, Vec<String>>,
}

/// Extract task-scoped timeline fields from a graph produced by
/// [`crate::graph_export::export_graph_for_task`].
pub(super) fn episode_metadata_from_task_graph(graph: &ExportedGraph) -> EpisodeTaskGraphMeta {
    let mut meta = EpisodeTaskGraphMeta::default();

    let task_exec_label = GraphNodeLabel::TaskExecution.as_str();
    let task_state_label = GraphNodeLabel::TaskState.as_str();
    let artifact_label = GraphNodeLabel::Artifact.as_str();
    let intent_label = GraphNodeLabel::Intent.as_str();

    for n in &graph.nodes {
        let p = &n.properties;
        match n.label.as_str() {
            l if l == task_exec_label => {
                let ts = prop_u64(p, &[a2a::TIMESTAMP_MS, "a2a:timestamp_ms"]);
                if let Some(t) = ts {
                    meta.task_start_ms = Some(match meta.task_start_ms {
                        Some(cur) => cur.min(t),
                        None => t,
                    });
                }
            }
            l if l == task_state_label => {
                let new_s = prop_str(p, &[a2a::TASK_STATE, "a2a:task_state"]).unwrap_or_default();
                if new_s.is_empty() {
                    continue;
                }
                let ts = prop_u64(p, &[a2a::TASK_STATE_TIME, "a2a:task_state_time"])
                    .or_else(|| prop_u64(p, &[a2a::TIMESTAMP_MS, "a2a:timestamp_ms"]))
                    .unwrap_or(0);
                let anchor =
                    prop_str(p, &[a2a::ACTIVITY_ANCHOR, "a2a:activity_anchor"]).unwrap_or_default();
                let old_s = prop_str(p, &[a2a::OLD_STATUS, "a2a:old_status"]).unwrap_or_default();

                meta.status_rows.push(StatusRow {
                    timestamp_ms: ts,
                    event_order: n.event_order.unwrap_or(0),
                    activity_anchor: anchor,
                    old_status: old_s,
                    new_status: new_s.clone(),
                });

                let replace_terminal = match &meta.terminal {
                    None => true,
                    Some((_, t0)) => ts >= *t0,
                };
                if is_terminal_state_str(&new_s) && replace_terminal {
                    meta.terminal = Some((new_s, ts));
                }
            }
            l if l == artifact_label => {
                let anchor =
                    prop_str(p, &[a2a::ACTIVITY_ANCHOR, "a2a:activity_anchor"]).unwrap_or_default();
                let name = prop_str(p, &[a2a::ARTIFACT_ID, "a2a:artifact_id"])
                    .or_else(|| prop_str(p, &[prov::LABEL, "prov:label"]))
                    .unwrap_or_else(|| "artifact".to_string());
                let media = prop_str(p, &[a2a::ARTIFACT_TYPE, "a2a:artifact_type"]);
                let ts = prop_u64(p, &[a2a::TIMESTAMP_MS, "a2a:timestamp_ms"]).unwrap_or(0);
                meta.artifact_rows.push(ArtifactRow {
                    timestamp_ms: ts,
                    event_order: n.event_order.unwrap_or(0),
                    activity_anchor: anchor,
                    name,
                    media_type: media,
                });
            }
            l if l == intent_label => {
                let anchor =
                    prop_str(p, &[a2a::ACTIVITY_ANCHOR, "a2a:activity_anchor"]).unwrap_or_default();
                if anchor.is_empty() {
                    continue;
                }
                let cites: Vec<String> = p
                    .get("citations")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if !cites.is_empty() {
                    meta.intent_citations.insert(anchor, cites);
                }
            }
            _ => {}
        }
    }

    meta.status_rows.sort_by(|a, b| {
        a.timestamp_ms
            .cmp(&b.timestamp_ms)
            .then_with(|| a.activity_anchor.cmp(&b.activity_anchor))
    });
    meta.artifact_rows.sort_by(|a, b| {
        a.timestamp_ms
            .cmp(&b.timestamp_ms)
            .then_with(|| a.activity_anchor.cmp(&b.activity_anchor))
    });

    meta
}

fn prop_str(props: &HashMap<String, Value>, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = props.get(*k).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

fn prop_u64(props: &HashMap<String, Value>, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(n) = props.get(*k).and_then(Value::as_u64) {
            return Some(n);
        }
    }
    None
}

fn is_terminal_state_str(s: &str) -> bool {
    // Wire and normalizer use mixed case (e.g. `completed` from TaskStatusChanged).
    let n = s.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "task_state_completed"
            | "completed"
            | "task_state_failed"
            | "failed"
            | "task_state_canceled"
            | "canceled"
            | "task_state_cancelled"
            | "cancelled"
            | "task_state_rejected"
            | "rejected"
    )
}
