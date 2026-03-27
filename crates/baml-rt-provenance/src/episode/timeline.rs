//! Merged timeline for episode assembly (conversation + status + artifacts).

use super::from_graph::{ArtifactRow, StatusRow};
use crate::store::ProvenanceConversationContextItem;

#[derive(Debug, Clone)]
pub(crate) enum TimelineKind {
    Conv(ProvenanceConversationContextItem, bool),
    Status(StatusRow),
    Artifact(ArtifactRow),
}
