use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("provenance storage error: {0}")]
    Storage(#[from] Box<dyn std::error::Error + Send + Sync>),
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
}

pub type Result<T> = std::result::Result<T, ProvenanceError>;
