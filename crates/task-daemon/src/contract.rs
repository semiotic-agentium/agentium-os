//! Event formats used when task-daemon hands work to interpreters and
//! downstream agents.
//!
//! These types help integrators follow one piece of work from the source
//! material that was polled, through interpretation, to the tasks produced
//! from it.

use baml_rt_core::ids::{ContextId, CorrelationId, EventId, TaskId};
use serde::{Deserialize, Serialize};

use crate::model::{
    InvestigationTask, ProjectContext, ProjectInterpretation, SlackMessage, TaskBatch,
    TaskSourceKind, unix_now,
};

/// Event format name used by task-daemon interpretation messages.
pub const INTERPRETATION_EVENT_SCHEMA_VERSION: &str = "task-daemon.interpretation.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Where a task-daemon event came from.
pub struct ContractSource {
    /// Stable identifier for the source, used to resume polling safely.
    pub source_key: String,
    /// Source category, such as Slack or ClickUp.
    pub source: TaskSourceKind,
    /// Human-readable source label (for example `#agentium-eng`).
    pub source_label: String,
}

impl ContractSource {
    /// Creates a source descriptor for one event.
    pub fn new(source_key: String, source: TaskSourceKind, source_label: String) -> Self {
        Self {
            source_key,
            source,
            source_label,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
/// Optional links that help operators trace a result back to the run that produced it.
pub struct ContractProvenance {
    /// Context identifier for the surrounding run, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    /// Task identifier for task-scoped runs, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Correlation identifier used to link related operations together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    /// Parent event id used to show which earlier event this one came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<EventId>,
    /// Source cursor used for this poll window.
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
/// Event sent to the component that interprets a poll window.
pub struct InterpretationRequestEvent {
    /// Event format name.
    pub schema_version: String,
    /// Stable identifier for this request.
    pub event_id: String,
    /// Unix timestamp for when the event was emitted.
    pub emitted_at_unix: u64,
    /// Where this work came from.
    pub source: ContractSource,
    /// Project context attached to the event.
    pub project: ProjectContext,
    /// Optional links back to the surrounding run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ContractProvenance>,
    /// Source messages to interpret.
    pub messages: Vec<SlackMessage>,
}

impl InterpretationRequestEvent {
    /// Creates a request event from one polled source window.
    pub fn from_source_poll(
        poll: &crate::daemon::SourcePoll,
        project: ProjectContext,
        provenance: Option<ContractProvenance>,
    ) -> Self {
        Self::new(
            ContractSource::new(
                poll.source_key.clone(),
                poll.source_kind(),
                poll.source_label.clone(),
            ),
            project,
            poll.messages().to_vec(),
            provenance,
        )
    }

    /// Creates a request event from one poll window.
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
/// Event produced after a poll window has been interpreted.
pub struct InterpretationResultEvent {
    /// Event format name.
    pub schema_version: String,
    /// Stable identifier for this result.
    pub event_id: String,
    /// Request event id this result was derived from.
    pub request_event_id: String,
    /// Unix timestamp for when the result was emitted.
    pub emitted_at_unix: u64,
    /// Where this work came from.
    pub source: ContractSource,
    /// Project context used while producing the result.
    pub project: ProjectContext,
    /// Number of source messages included in this interpretation.
    pub messages_scanned: usize,
    /// Structured understanding of the source material.
    pub interpretation: ProjectInterpretation,
    /// Tasks or follow-up work produced from the interpretation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_tasks: Vec<InvestigationTask>,
    /// Optional links back to the surrounding run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ContractProvenance>,
}

impl InterpretationResultEvent {
    /// Creates a result event from a task batch produced for a request event.
    pub fn from_batch(request: &InterpretationRequestEvent, batch: &TaskBatch) -> Self {
        Self::from_request(
            request,
            batch.interpretation.clone(),
            batch.derived_tasks.clone(),
        )
    }

    /// Creates a result event from an interpretation request and its output.
    pub fn from_request(
        request: &InterpretationRequestEvent,
        interpretation: ProjectInterpretation,
        derived_tasks: Vec<InvestigationTask>,
    ) -> Self {
        let event_id = result_event_id(&request.event_id, &interpretation, &derived_tasks);
        let provenance = request.provenance.clone().map(|mut value| {
            value.parent_event_id = Some(EventId::from(request.event_id.clone()));
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

#[derive(Debug, Clone, PartialEq)]
/// Everything a sink receives for one daemon result.
pub struct TaskDispatch {
    pub request_event: InterpretationRequestEvent,
    pub result_event: InterpretationResultEvent,
    pub batch: TaskBatch,
}

impl TaskDispatch {
    pub fn new(
        request_event: InterpretationRequestEvent,
        result_event: InterpretationResultEvent,
        batch: TaskBatch,
    ) -> Self {
        Self {
            request_event,
            result_event,
            batch,
        }
    }

    pub fn from_batch(request_event: InterpretationRequestEvent, batch: TaskBatch) -> Self {
        let result_event = InterpretationResultEvent::from_batch(&request_event, &batch);
        Self::new(request_event, result_event, batch)
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
                context_id: Some(ContextId::new(1, 1)),
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
                context_id: Some(ContextId::new(1, 1)),
                correlation_id: Some(CorrelationId::new(1, 1)),
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
            provenance.parent_event_id.as_ref().map(|id| id.as_str()),
            Some(request.event_id.as_str())
        );
        assert_eq!(
            provenance.context_id.as_ref().map(|id| id.as_str()),
            Some(ContextId::new(1, 1).as_str())
        );
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
