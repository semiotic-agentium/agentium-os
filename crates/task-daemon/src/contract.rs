//! Versioned event contracts for the task-daemon interpretation pipeline.
//!
//! These envelopes formalize the handoff boundary:
//! - polling output -> interpretation input (`InterpretationRequestEvent`)
//! - interpretation output -> orchestration/sink input (`InterpretationResultEvent`)
//!
//! The contract carries provenance identifiers so downstream systems can stitch
//! together Slack evidence, interpretation runs, and generated tasks.

use serde::{Deserialize, Serialize};

use crate::model::{
    InvestigationTask, ProjectContext, ProjectInterpretation, SlackMessage, TaskBatch,
    TaskSourceKind, unix_now,
};

/// Schema identifier for the interpretation handoff contract.
pub const INTERPRETATION_EVENT_SCHEMA_VERSION: &str = "task-daemon.interpretation.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Source identity for a contract event.
pub struct ContractSource {
    /// Stable source state key used by the daemon state store.
    pub source_key: String,
    /// Source category for routing logic.
    pub source: TaskSourceKind,
    /// Human-readable source label (for example `#agentium-eng`).
    pub source_label: String,
}

impl ContractSource {
    /// Builds a source descriptor.
    pub fn new(source_key: String, source: TaskSourceKind, source_label: String) -> Self {
        Self {
            source_key,
            source,
            source_label,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
/// Provenance metadata used to correlate contract events with runtime traces.
pub struct ContractProvenance {
    /// Runtime context identifier (typically provenance context id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Runtime task identifier when a task-scoped flow exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Correlation identifier (for example request id) for distributed tracing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Parent event id used to preserve causality between request/result events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    /// Source cursor used for this poll window (for example latest Slack ts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<String>,
    /// Message timestamps included in this poll window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_message_ts: Vec<String>,
}

impl ContractProvenance {
    fn is_empty(&self) -> bool {
        self.context_id.is_none()
            && self.task_id.is_none()
            && self.correlation_id.is_none()
            && self.parent_event_id.is_none()
            && self.source_cursor.is_none()
            && self.source_message_ts.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Input event consumed by interpretation backends.
pub struct InterpretationRequestEvent {
    /// Versioned schema identifier.
    pub schema_version: String,
    /// Stable id for this request event.
    pub event_id: String,
    /// Unix timestamp when the event envelope was emitted.
    pub emitted_at_unix: u64,
    /// Source metadata for routing and replay.
    pub source: ContractSource,
    /// Resolved project context.
    pub project: ProjectContext,
    /// Runtime/provenance correlation metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ContractProvenance>,
    /// Normalized Slack messages for interpretation.
    pub messages: Vec<SlackMessage>,
}

impl InterpretationRequestEvent {
    /// Builds a request event from [`crate::daemon::SourcePoll`].
    pub fn from_source_poll(
        poll: &crate::daemon::SourcePoll,
        project: ProjectContext,
        provenance: Option<ContractProvenance>,
    ) -> Self {
        Self::new(
            ContractSource::new(
                poll.source_key.clone(),
                poll.source,
                poll.source_label.clone(),
            ),
            project,
            poll.messages.clone(),
            provenance,
        )
    }

    /// Builds a request event from one poll window.
    pub fn new(
        source: ContractSource,
        project: ProjectContext,
        messages: Vec<SlackMessage>,
        provenance: Option<ContractProvenance>,
    ) -> Self {
        let normalized_provenance = normalize_provenance(provenance, &messages);
        let event_id =
            request_event_id(&source, &project, &messages, normalized_provenance.as_ref());

        Self {
            schema_version: INTERPRETATION_EVENT_SCHEMA_VERSION.to_string(),
            event_id,
            emitted_at_unix: unix_now(),
            source,
            project,
            provenance: normalized_provenance,
            messages,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Output event produced by interpretation backends.
pub struct InterpretationResultEvent {
    /// Versioned schema identifier.
    pub schema_version: String,
    /// Stable id for this result event.
    pub event_id: String,
    /// Request event id this result was derived from.
    pub request_event_id: String,
    /// Unix timestamp when the result envelope was emitted.
    pub emitted_at_unix: u64,
    /// Source metadata for routing and replay.
    pub source: ContractSource,
    /// Project context used by interpretation.
    pub project: ProjectContext,
    /// Number of source messages included in this interpretation.
    pub messages_scanned: usize,
    /// Structured interpretation payload.
    pub interpretation: ProjectInterpretation,
    /// Derived downstream tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_tasks: Vec<InvestigationTask>,
    /// Runtime/provenance correlation metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ContractProvenance>,
}

impl InterpretationResultEvent {
    /// Builds a result event from a [`TaskBatch`] generated for a request event.
    pub fn from_batch(request: &InterpretationRequestEvent, batch: &TaskBatch) -> Self {
        Self::from_request(
            request,
            batch.interpretation.clone(),
            batch.derived_tasks.clone(),
        )
    }

    /// Builds a result event from an interpretation request and extracted payloads.
    pub fn from_request(
        request: &InterpretationRequestEvent,
        interpretation: ProjectInterpretation,
        derived_tasks: Vec<InvestigationTask>,
    ) -> Self {
        let event_id = result_event_id(&request.event_id, &interpretation, &derived_tasks);
        let provenance = request.provenance.clone().map(|mut value| {
            value.parent_event_id = Some(request.event_id.clone());
            value
        });

        Self {
            schema_version: INTERPRETATION_EVENT_SCHEMA_VERSION.to_string(),
            event_id,
            request_event_id: request.event_id.clone(),
            emitted_at_unix: unix_now(),
            source: request.source.clone(),
            project: request.project.clone(),
            messages_scanned: request.messages.len(),
            interpretation,
            derived_tasks,
            provenance,
        }
    }
}

fn normalize_provenance(
    provenance: Option<ContractProvenance>,
    messages: &[SlackMessage],
) -> Option<ContractProvenance> {
    let mut value = provenance.unwrap_or_default();
    if value.source_message_ts.is_empty() {
        value.source_message_ts = messages.iter().map(|message| message.ts.clone()).collect();
    }
    if value.is_empty() { None } else { Some(value) }
}

fn request_event_id(
    source: &ContractSource,
    project: &ProjectContext,
    messages: &[SlackMessage],
    provenance: Option<&ContractProvenance>,
) -> String {
    let message_count = messages.len().to_string();
    let first_ts = messages
        .first()
        .map(|message| message.ts.as_str())
        .unwrap_or("");
    let last_ts = messages
        .last()
        .map(|message| message.ts.as_str())
        .unwrap_or("");
    let source_cursor = provenance
        .and_then(|value| value.source_cursor.as_deref())
        .unwrap_or("");

    hash_event_id(
        "td-interpret-request",
        &[
            source.source_key.as_str(),
            source.source_label.as_str(),
            project.project_key.as_str(),
            first_ts,
            last_ts,
            message_count.as_str(),
            source_cursor,
        ],
    )
}

fn result_event_id(
    request_event_id: &str,
    interpretation: &ProjectInterpretation,
    derived_tasks: &[InvestigationTask],
) -> String {
    let mut keys: Vec<&str> = derived_tasks.iter().map(|task| task.key.as_str()).collect();
    keys.sort_unstable();
    let task_key_blob = keys.join(",");

    hash_event_id(
        "td-interpret-result",
        &[
            request_event_id,
            interpretation.executive_summary.as_str(),
            task_key_blob.as_str(),
        ],
    )
}

fn hash_event_id(prefix: &str, parts: &[&str]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001B3;

    fn fnv1a_extend(mut hash: u64, bytes: &[u8]) -> u64 {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    let mut digest = FNV_OFFSET_BASIS;
    for part in parts {
        digest = fnv1a_extend(digest, part.as_bytes());
        digest = fnv1a_extend(digest, &[0x1f]);
    }

    format!("{prefix}-{digest:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ProjectInterpretation, SourceReference, TaskConfidence, TaskSourceKind, WorkflowSeed,
    };

    fn sample_source() -> ContractSource {
        ContractSource::new(
            "slack:C123".to_string(),
            TaskSourceKind::Slack,
            "#agentium-eng".to_string(),
        )
    }

    fn sample_project() -> ProjectContext {
        ProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: true,
            repo_path: Some("/repo/agent-platform".to_string()),
        }
    }

    fn sample_message(ts: &str, text: &str) -> SlackMessage {
        SlackMessage {
            channel_name: "agentium-eng".to_string(),
            channel_id: "C123".to_string(),
            ts: ts.to_string(),
            thread_ts: None,
            user_id: Some("U123".to_string()),
            user_name: Some("alice".to_string()),
            text: text.to_string(),
            subtype: None,
            source: SourceReference {
                reference: format!("slack://channel/C123/p{}", ts.replace('.', "")),
                permalink: None,
                channel_id: Some("C123".to_string()),
                message_ts: Some(ts.to_string()),
                thread_ts: None,
            },
        }
    }

    #[test]
    fn request_event_id_is_stable_for_same_poll_window() {
        let messages = vec![
            sample_message("1735689600.000000", "first"),
            sample_message("1735689700.000000", "second"),
        ];
        let request_one = InterpretationRequestEvent::new(
            sample_source(),
            sample_project(),
            messages.clone(),
            None,
        );
        let request_two =
            InterpretationRequestEvent::new(sample_source(), sample_project(), messages, None);

        assert_eq!(request_one.event_id, request_two.event_id);
        assert_eq!(
            request_one.schema_version,
            INTERPRETATION_EVENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn request_populates_provenance_message_timestamps() {
        let request = InterpretationRequestEvent::new(
            sample_source(),
            sample_project(),
            vec![
                sample_message("1735689600.000000", "first"),
                sample_message("1735689700.000000", "second"),
            ],
            Some(ContractProvenance {
                context_id: Some("ctx-1".to_string()),
                source_cursor: Some("1735689700.000000".to_string()),
                ..ContractProvenance::default()
            }),
        );

        let provenance = request.provenance.expect("provenance exists");
        assert_eq!(provenance.source_message_ts.len(), 2);
        assert_eq!(provenance.source_message_ts[0], "1735689600.000000");
        assert_eq!(provenance.source_message_ts[1], "1735689700.000000");
    }

    #[test]
    fn result_event_inherits_and_links_provenance() {
        let request = InterpretationRequestEvent::new(
            sample_source(),
            sample_project(),
            vec![sample_message("1735689600.000000", "first")],
            Some(ContractProvenance {
                context_id: Some("ctx-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                ..ContractProvenance::default()
            }),
        );
        let result = InterpretationResultEvent::from_request(
            &request,
            ProjectInterpretation {
                executive_summary: "Interpretation summary".to_string(),
                workflow_seed: WorkflowSeed {
                    goal: "Investigate".to_string(),
                    ..WorkflowSeed::default()
                },
                ..ProjectInterpretation::default()
            },
            vec![InvestigationTask {
                key: "prompt-1".to_string(),
                title: "Investigate cursor handling".to_string(),
                description: "Check poll and delivery ordering".to_string(),
                priority: TaskConfidence::High,
                sources: Vec::new(),
            }],
        );

        assert_eq!(result.request_event_id, request.event_id);
        assert_eq!(result.messages_scanned, 1);
        let provenance = result.provenance.expect("provenance exists");
        assert_eq!(
            provenance.parent_event_id.as_deref(),
            Some(request.event_id.as_str())
        );
        assert_eq!(provenance.context_id.as_deref(), Some("ctx-1"));
    }

    #[test]
    fn result_event_serialization_contains_expected_shape() {
        let request = InterpretationRequestEvent::new(
            sample_source(),
            sample_project(),
            vec![sample_message("1735689600.000000", "first")],
            None,
        );
        let result = InterpretationResultEvent::from_request(
            &request,
            ProjectInterpretation {
                executive_summary: "Summary".to_string(),
                ..ProjectInterpretation::default()
            },
            Vec::new(),
        );
        let json = serde_json::to_value(result).expect("serialize result event");

        assert_eq!(
            json.get("schema_version")
                .and_then(serde_json::Value::as_str),
            Some(INTERPRETATION_EVENT_SCHEMA_VERSION)
        );
        assert_eq!(
            json.get("request_event_id")
                .and_then(serde_json::Value::as_str),
            Some(request.event_id.as_str())
        );
    }

    #[test]
    fn result_event_can_be_built_from_task_batch() {
        let request = InterpretationRequestEvent::new(
            sample_source(),
            sample_project(),
            vec![sample_message("1735689600.000000", "first")],
            None,
        );
        let batch = TaskBatch {
            source: TaskSourceKind::Slack,
            source_label: "#agentium-eng".to_string(),
            generated_at_unix: 1_735_689_700,
            messages_scanned: 1,
            project: sample_project(),
            interpretation: ProjectInterpretation {
                executive_summary: "Summary".to_string(),
                ..ProjectInterpretation::default()
            },
            derived_tasks: vec![InvestigationTask {
                key: "prompt-1".to_string(),
                title: "Investigate".to_string(),
                description: "Inspect behavior".to_string(),
                priority: TaskConfidence::Medium,
                sources: Vec::new(),
            }],
        };

        let result = InterpretationResultEvent::from_batch(&request, &batch);
        assert_eq!(result.request_event_id, request.event_id);
        assert_eq!(result.derived_tasks.len(), 1);
    }
}
