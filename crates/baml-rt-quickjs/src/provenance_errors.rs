// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Map provenance crate errors into [`BamlRtError`](baml_rt_core::BamlRtError) for archive I/O.

use baml_rt_core::BamlRtError;
use baml_rt_provenance::ProvenanceError;

pub(crate) fn map_archive_provenance_err(e: ProvenanceError) -> BamlRtError {
    match e {
        ProvenanceError::Contention { details } => BamlRtError::Conflict(format!(
            "archive ref provenance contention (retry): {details}"
        )),
        ProvenanceError::Storage(source) => BamlRtError::InvalidArgumentWithSource {
            message: "archive ref provenance storage".into(),
            source,
        },
        ProvenanceError::CorruptArchiveEntry { .. } | ProvenanceError::CorruptPayloadRow { .. } => {
            BamlRtError::InvalidArgument(format!("archive ref corrupt row: {e}"))
        }
        ProvenanceError::InvalidEvent {
            activity_anchor,
            reason,
        } => BamlRtError::InvalidArgument(format!(
            "archive ref provenance ({activity_anchor}): {reason}"
        )),
        ProvenanceError::MissingField {
            activity_anchor,
            field,
        } => BamlRtError::InvalidArgument(format!(
            "archive ref provenance ({activity_anchor}): missing field {field}"
        )),
        ProvenanceError::InvalidMapping {
            relation,
            from_label,
            to_label,
        } => BamlRtError::InvalidArgument(format!(
            "archive ref provenance invalid mapping {relation} ({from_label} -> {to_label})"
        )),
        ProvenanceError::MissingLabel { node_id, kind } => BamlRtError::InvalidArgument(format!(
            "archive ref provenance missing label for {kind} {node_id}"
        )),
        ProvenanceError::MessageActivityAnchorConflict {
            activity_anchor,
            context_id,
            existing_node_id,
            expected_entity_id,
        } => BamlRtError::InvalidArgument(format!(
            "archive ref provenance message anchor conflict {activity_anchor} in {context_id}: \
             existing node {existing_node_id}, expected {expected_entity_id}"
        )),
    }
}
