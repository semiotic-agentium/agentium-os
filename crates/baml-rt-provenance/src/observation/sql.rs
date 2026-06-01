// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared Surreal filter fragments for observation reads.

/// Filter clause for rows after an exclusive event order.
#[must_use]
pub fn after_event_order_filter_sql() -> &'static str {
    "AND props.a2a_event_order > $after_event_order"
}
