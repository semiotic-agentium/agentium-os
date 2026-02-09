use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("provenance storage error")]
    Storage(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("invalid provenance event {event_id}: {reason}")]
    InvalidEvent { event_id: String, reason: String },
    #[error("missing required field in event {event_id}: {field}")]
    MissingField { event_id: String, field: String },
    #[error("invalid provenance mapping: {relation} ({from_label} -> {to_label})")]
    InvalidMapping {
        relation: String,
        from_label: String,
        to_label: String,
    },
    #[error("missing required label for {kind} {node_id}")]
    MissingLabel { node_id: String, kind: String },
}

pub type Result<T> = std::result::Result<T, ProvenanceError>;
