// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Merged timeline for episode assembly (conversation + status + artifacts).

use crate::view::ProvenanceConversationContextItem;

#[derive(Debug, Clone)]
pub struct StatusRow {
    pub timestamp_ms: u64,
    pub event_order: u64,
    pub activity_anchor: String,
    pub old_status: String,
    pub new_status: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactRow {
    pub timestamp_ms: u64,
    pub event_order: u64,
    pub activity_anchor: String,
    pub name: String,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone)]
pub enum TimelineKind {
    Conv(ProvenanceConversationContextItem, bool),
    Status(StatusRow),
    Artifact(ArtifactRow),
}
