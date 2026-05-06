use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("provenance storage error: {0}")]
    Storage(#[from] Box<dyn std::error::Error + Send + Sync>),
    /// Concurrent writers contended on a shared record (agent runtime instance,
    /// context entity, etc.) and the bounded retry budget was exhausted. Distinct
    /// from [`ProvenanceError::Storage`] so the host can re-queue rather than
    /// classifying the failure as `LlmCorrectable`.
    #[error("provenance write contention exhausted retries: {details}")]
    Contention { details: String },
    #[error("invalid provenance activity anchor {activity_anchor}: {reason}")]
    InvalidEvent {
        activity_anchor: String,
        reason: String,
    },
    #[error("missing required field for activity anchor {activity_anchor}: {field}")]
    MissingField {
        activity_anchor: String,
        field: String,
    },
    #[error("invalid provenance mapping: {relation} ({from_label} -> {to_label})")]
    InvalidMapping {
        relation: String,
        from_label: String,
        to_label: String,
    },
    #[error("missing required label for {kind} {node_id}")]
    MissingLabel { node_id: String, kind: String },
    #[error("corrupt provenance_payload row: {reason}")]
    CorruptPayloadRow { reason: String },
    /// `archive_body.entry` JSON could not be decoded into `ArchiveEntry` (tools crate).
    #[error("corrupt archive_body.entry: {reason}")]
    CorruptArchiveEntry { reason: String },
    /// Another `Message` graph node already owns this activity anchor in the scoped context.
    /// Emission must be idempotent (same `node_id`); conflicting rows indicate a host/emitter bug.
    #[error(
        "message activity anchor {activity_anchor} already bound to graph node {existing_node_id} \
         in context {context_id}; this write expects entity {expected_entity_id}"
    )]
    MessageActivityAnchorConflict {
        activity_anchor: String,
        context_id: String,
        existing_node_id: String,
        expected_entity_id: String,
    },
}

pub type Result<T> = std::result::Result<T, ProvenanceError>;
