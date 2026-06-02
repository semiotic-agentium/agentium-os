// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed `messages[]` body for `system.callback.v1` host dispatch (matches callback producer).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::callback_producer::{CALLBACK_EVENT_SCHEMA_VERSION, CALLBACK_SOURCE_KIND};

/// Source metadata on a callback dispatch message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct SystemCallbackDispatchSource {
    pub source_kind: String,
    pub source_key: String,
}

/// Request routing metadata echoed on callback dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, JsonSchema)]
pub struct SystemCallbackDispatchRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling_context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requesting_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requesting_message_id: Option<String>,
}

/// One `messages[]` item emitted by the system/callback producer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct SystemCallbackDispatchMessage {
    pub schema_version: String,
    pub callback_id: String,
    pub source: SystemCallbackDispatchSource,
    pub scheduled_for_unix_ms: u64,
    pub requested_at_unix_ms: u64,
    pub emitted_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    pub payload: Value,
    #[serde(default)]
    pub request: SystemCallbackDispatchRequest,
}

impl SystemCallbackDispatchMessage {
    /// Minimal operator-console sample (token probe) with required envelope fields.
    pub fn token_probe_sample(source_key: &str, token: &str) -> Self {
        let now = 1_735_720_000u64;
        Self {
            schema_version: CALLBACK_EVENT_SCHEMA_VERSION.to_string(),
            callback_id: "probe-callback".to_string(),
            source: SystemCallbackDispatchSource {
                source_kind: CALLBACK_SOURCE_KIND.to_string(),
                source_key: source_key.to_string(),
            },
            scheduled_for_unix_ms: now,
            requested_at_unix_ms: now,
            emitted_at_unix_ms: now,
            dedupe_key: None,
            payload: serde_json::json!({ "token": token }),
            request: SystemCallbackDispatchRequest::default(),
        }
    }
}

/// JSON Schema for [`SystemCallbackDispatchMessage`].
pub fn system_callback_dispatch_json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(SystemCallbackDispatchMessage))
        .expect("SystemCallbackDispatchMessage schema serializes to JSON")
}
