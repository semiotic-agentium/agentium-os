// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON Schema exports for agent-deliverable Slack source-record batches.

use serde_json::Value;

use crate::normalize::SlackNormalizedBatch;

/// JSON Schema for one [`SlackNormalizedBatch`] message body (`messages[]` item).
pub fn slack_normalized_batch_json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(SlackNormalizedBatch))
        .expect("SlackNormalizedBatch schema serializes to JSON")
}
