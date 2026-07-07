// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON Schema for Settings / `PUT /config/semiotic` validation.

use serde_json::Value;

/// Minimal JSON Schema for [`crate::config::SemioticConfig`].
pub fn semiotic_bundle_schema() -> Value {
    let policy = serde_json::json!({
        "type": "object",
        "properties": {
            "enabled": { "type": "boolean" },
            "mode": { "type": "string", "enum": ["dry_run", "enforce"] },
            "enforceMinTier": { "type": "integer", "minimum": 0, "maximum": 3 },
            "requirePostconditionsT3": { "type": "boolean" },
            "strictCitationAnchors": { "type": "boolean" }
        }
    });
    serde_json::json!({
        "type": "object",
        "properties": {
            "enabled": { "type": "boolean" },
            "mode": { "type": "string", "enum": ["dry_run", "enforce"] },
            "enforceMinTier": { "type": "integer", "minimum": 0, "maximum": 3 },
            "requirePostconditionsT3": { "type": "boolean" },
            "strictCitationAnchors": { "type": "boolean" },
            "overrides": {
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "object",
                        "additionalProperties": policy
                    }
                }
            }
        }
    })
}
