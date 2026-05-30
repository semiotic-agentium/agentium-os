// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Deterministic `provenance_payload.payload_id` strings shared by the normalizer, context reader,
//! and payload store (`payload:{activity_anchor}:{payload_kind}`).

/// Default line cap when graph nodes omit `read_limit` (legacy rows).
pub const DEFAULT_SESSION_READ_LINE_LIMIT: usize = 200;

#[must_use]
#[inline]
pub(crate) fn payload_row_id(activity_anchor: &str, payload_kind: &str) -> String {
    format!("payload:{activity_anchor}:{payload_kind}")
}
