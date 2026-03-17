//! Event formats used when task-daemon hands work to interpreters and
//! downstream agents.
//!
//! These types help integrators follow one piece of work from the source
//! material that was polled, through interpretation, to the tasks produced
//! from it.

use baml_rt_core::ids::{ContextId, CorrelationId, DigestIdParts, EventId, TaskId};
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
        // Mint cursor/root fields before `Self::new`; `normalize_provenance`
        // later fills message timestamps, so the two steps stay independent.
        let provenance = provenance_for_source_poll(poll, provenance);
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
            batch.messages_scanned,
            batch.interpretation.clone(),
            batch.derived_tasks.clone(),
        )
    }

    /// Creates a result event from an interpretation request and its output.
    pub fn from_request(
        request: &InterpretationRequestEvent,
        messages_scanned: usize,
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
            messages_scanned,
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

fn provenance_for_source_poll(
    poll: &crate::daemon::SourcePoll,
    provenance: Option<ContractProvenance>,
) -> Option<ContractProvenance> {
    let mut value = provenance.unwrap_or_default();

    if value.source_cursor.is_none() {
        value.source_cursor = poll.source_cursor().map(ToOwned::to_owned);
    }

    let Some(source_cursor) = value.source_cursor.as_deref() else {
        // Without either a caller-supplied cursor or a poll-derived cursor,
        // there is nothing stable to root new provenance ids against. Preserve
        // any preseeded provenance fields and skip minting.
        return if value.is_empty() { None } else { Some(value) };
    };

    // External events should get a stable root the first time task-daemon sees
    // them so retries preserve the same provenance chain. Labels are
    // descriptive and may change independently of source identity, so they are
    // intentionally excluded from the root seed.
    let seed_parts = [
        poll.source_kind().as_str(),
        poll.source_key.as_str(),
        source_cursor,
    ];

    if value.context_id.is_none() {
        let parts = stable_id_parts("td-external-context", &seed_parts);
        value.context_id = Some(ContextId::from_digest_parts(parts));
    }
    if value.correlation_id.is_none() {
        let parts = stable_id_parts("td-external-correlation", &seed_parts);
        value.correlation_id = Some(CorrelationId::from_digest_parts(parts));
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

    // Request ids should stay stable for the same logical poll window even if
    // the human-readable source label changes between retries.
    hash_event_id(
        "td-interpret-request",
        &[
            source.source_key.as_str(),
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
    let digest = hash_digest(parts);
    format!("{prefix}-{digest:016x}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StableIdLane {
    Upper,
    Lower,
}

impl StableIdLane {
    const fn as_str(self) -> &'static str {
        match self {
            // These lane names are part of the hash input. Renaming them is an
            // id-compatibility migration, not a harmless refactor.
            Self::Upper => "upper",
            Self::Lower => "lower",
        }
    }
}

fn stable_id_parts(namespace: &str, parts: &[&str]) -> DigestIdParts {
    // These ids need deterministic separation, not cryptographic randomness.
    // A namespaced/lane-split 64-bit FNV-1a digest keeps collision risk
    // negligible for task-daemon poll volumes, and `.max(1)` avoids zero
    // components in the typed ids we mint from it.
    let upper_raw = hash_digest_with_namespace(namespace, StableIdLane::Upper, parts);
    let lower_raw = hash_digest_with_namespace(namespace, StableIdLane::Lower, parts);
    debug_assert_ne!(
        upper_raw, 0,
        "FNV digest unexpectedly produced zero for upper lane"
    );
    debug_assert_ne!(
        lower_raw, 0,
        "FNV digest unexpectedly produced zero for lower lane"
    );
    let upper = upper_raw.max(1);
    let lower = lower_raw.max(1);
    DigestIdParts::new(upper, lower)
}

fn hash_digest_with_namespace(namespace: &str, lane: StableIdLane, parts: &[&str]) -> u64 {
    let mut digest = hash_digest(&[namespace, lane.as_str()]);
    for part in parts {
        digest = hash_digest_extend(digest, part);
    }
    digest
}

fn hash_digest(parts: &[&str]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

    let mut digest = FNV_OFFSET_BASIS;
    for part in parts {
        digest = hash_digest_extend(digest, part);
    }
    digest
}

fn hash_digest_extend(mut digest: u64, part: &str) -> u64 {
    const FNV_PRIME: u64 = 0x00000100000001B3;
    const HASH_PART_SEPARATOR: u8 = 0x1f;

    for &byte in part.as_bytes() {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(FNV_PRIME);
    }
    digest ^= u64::from(HASH_PART_SEPARATOR);
    digest.wrapping_mul(FNV_PRIME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::SourcePoll,
        model::{
            ProjectInterpretation, SourceReference, TaskConfidence, TaskSourceKind, WorkflowSeed,
        },
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
    fn from_source_poll_mints_stable_provenance_root_for_slack_messages() {
        let poll = SourcePoll::slack(
            "slack:C123".to_string(),
            "#agentium-eng".to_string(),
            vec![
                sample_message("1735689600.000000", "first"),
                sample_message("1735689700.000000", "second"),
            ],
            2,
        );

        let request_one =
            InterpretationRequestEvent::from_source_poll(&poll, sample_project(), None);
        let request_two =
            InterpretationRequestEvent::from_source_poll(&poll, sample_project(), None);

        let provenance_one = request_one.provenance.expect("provenance exists");
        let provenance_two = request_two.provenance.expect("provenance exists");
        assert_eq!(request_one.event_id, request_two.event_id);
        assert_eq!(provenance_one.context_id, provenance_two.context_id);
        assert_eq!(provenance_one.correlation_id, provenance_two.correlation_id);
        assert_eq!(
            provenance_one.source_cursor.as_deref(),
            Some("slack:1735689600.000000:1735689700.000000:2")
        );
    }

    #[test]
    fn from_source_poll_ignores_source_label_when_minting_provenance_root() {
        let messages = vec![
            sample_message("1735689600.000000", "first"),
            sample_message("1735689700.000000", "second"),
        ];
        let first_poll = SourcePoll::slack(
            "slack:C123".to_string(),
            "#agentium-eng".to_string(),
            messages.clone(),
            2,
        );
        let renamed_poll = SourcePoll::slack(
            "slack:C123".to_string(),
            "#eng-workflows".to_string(),
            messages,
            2,
        );

        let first_request =
            InterpretationRequestEvent::from_source_poll(&first_poll, sample_project(), None);
        let renamed_request =
            InterpretationRequestEvent::from_source_poll(&renamed_poll, sample_project(), None);

        let first_provenance = first_request.provenance.expect("first provenance");
        let renamed_provenance = renamed_request.provenance.expect("renamed provenance");
        assert_eq!(first_request.event_id, renamed_request.event_id);
        assert_eq!(first_provenance.context_id, renamed_provenance.context_id);
        assert_eq!(
            first_provenance.correlation_id,
            renamed_provenance.correlation_id
        );
        assert_eq!(
            first_provenance.source_cursor,
            renamed_provenance.source_cursor
        );
    }

    #[test]
    fn from_source_poll_uses_clickup_task_keys_to_distinguish_external_events() {
        let project = sample_project();
        let first_poll = SourcePoll::clickup(
            "clickup:list:901325431486".to_string(),
            "ClickUp list".to_string(),
            vec![InvestigationTask {
                key: "clickup-created:task-1:1".to_string(),
                title: "Investigate first task".to_string(),
                description: "First".to_string(),
                priority: TaskConfidence::High,
                sources: Vec::new(),
            }],
            1,
        );
        let second_poll = SourcePoll::clickup(
            "clickup:list:901325431486".to_string(),
            "ClickUp list".to_string(),
            vec![InvestigationTask {
                key: "clickup-created:task-2:1".to_string(),
                title: "Investigate second task".to_string(),
                description: "Second".to_string(),
                priority: TaskConfidence::High,
                sources: Vec::new(),
            }],
            1,
        );

        let first_request =
            InterpretationRequestEvent::from_source_poll(&first_poll, project.clone(), None);
        let second_request =
            InterpretationRequestEvent::from_source_poll(&second_poll, project, None);

        let first_provenance = first_request.provenance.expect("first provenance");
        let second_provenance = second_request.provenance.expect("second provenance");
        assert_ne!(first_request.event_id, second_request.event_id);
        assert_ne!(first_provenance.context_id, second_provenance.context_id);
        assert_ne!(
            first_provenance.source_cursor.as_deref(),
            second_provenance.source_cursor.as_deref()
        );
    }

    #[test]
    fn from_source_poll_preserves_preseeded_provenance_fields() {
        let poll = SourcePoll::slack(
            "slack:C123".to_string(),
            "#agentium-eng".to_string(),
            vec![
                sample_message("1735689600.000000", "first"),
                sample_message("1735689700.000000", "second"),
            ],
            2,
        );
        let provided_context = ContextId::new(42, 7);
        let provided_correlation = CorrelationId::new(24, 9);
        let provided_parent = EventId::from("prior-event".to_string());
        let provided_cursor = "custom-cursor".to_string();

        let request = InterpretationRequestEvent::from_source_poll(
            &poll,
            sample_project(),
            Some(ContractProvenance {
                context_id: Some(provided_context.clone()),
                correlation_id: Some(provided_correlation.clone()),
                parent_event_id: Some(provided_parent.clone()),
                source_cursor: Some(provided_cursor.clone()),
                ..ContractProvenance::default()
            }),
        );

        let provenance = request.provenance.expect("provenance exists");
        assert_eq!(provenance.context_id, Some(provided_context));
        assert_eq!(provenance.correlation_id, Some(provided_correlation));
        assert_eq!(provenance.parent_event_id, Some(provided_parent));
        assert_eq!(provenance.source_cursor, Some(provided_cursor));
        assert_eq!(
            provenance.source_message_ts,
            vec![
                "1735689600.000000".to_string(),
                "1735689700.000000".to_string()
            ]
        );
    }

    #[test]
    fn from_source_poll_without_any_cursor_preserves_preseeded_ids_only() {
        let poll = SourcePoll::slack(
            "slack:C123".to_string(),
            "#agentium-eng".to_string(),
            Vec::new(),
            0,
        );
        let provided_context = ContextId::new(42, 7);

        let request = InterpretationRequestEvent::from_source_poll(
            &poll,
            sample_project(),
            Some(ContractProvenance {
                context_id: Some(provided_context.clone()),
                ..ContractProvenance::default()
            }),
        );

        let provenance = request.provenance.expect("provenance exists");
        assert_eq!(provenance.context_id, Some(provided_context));
        assert_eq!(provenance.correlation_id, None);
        assert_eq!(provenance.source_cursor, None);
        assert!(provenance.source_message_ts.is_empty());
    }

    #[test]
    fn from_source_poll_pins_known_minted_ids_for_compatibility() {
        let poll = SourcePoll::slack(
            "slack:C123".to_string(),
            "#agentium-eng".to_string(),
            vec![
                sample_message("1735689600.000000", "first"),
                sample_message("1735689700.000000", "second"),
            ],
            2,
        );

        let request = InterpretationRequestEvent::from_source_poll(&poll, sample_project(), None);
        let provenance = request.provenance.expect("provenance exists");

        // These exact ids are part of the external compatibility surface for
        // task-daemon provenance. If they change, treat that as an id
        // migration and update the contract docs intentionally.
        assert_eq!(
            (
                request.event_id.as_str(),
                provenance.context_id.as_ref().map(ContextId::as_str),
                provenance
                    .correlation_id
                    .as_ref()
                    .map(CorrelationId::as_str),
            ),
            (
                "td-interpret-request-1b1eead226c93589",
                Some("ctx-7548386120284784534-8799862099676914443"),
                Some("corr-6129901457429418597-2178675600574945132"),
            )
        );
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
            request.messages.len(),
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
            request.messages.len(),
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

    #[test]
    fn result_event_from_batch_uses_batch_messages_scanned_for_non_message_sources() {
        let request =
            InterpretationRequestEvent::new(sample_source(), sample_project(), vec![], None);
        let batch = TaskBatch {
            source: TaskSourceKind::Clickup,
            source_label: "ClickUp list".to_string(),
            generated_at_unix: 1_735_689_700,
            messages_scanned: 4,
            project: sample_project(),
            interpretation: ProjectInterpretation {
                executive_summary: "Summary".to_string(),
                ..ProjectInterpretation::default()
            },
            derived_tasks: Vec::new(),
        };

        let result = InterpretationResultEvent::from_batch(&request, &batch);
        assert_eq!(result.messages_scanned, 4);
    }
}
