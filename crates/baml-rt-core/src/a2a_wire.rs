// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Newtype wrappers for A2A wire-level JSON so we don't pass bare `serde_json::Value`
//! at API boundaries. Documents intent and avoids mixing "any JSON" with "JSON-RPC request body"
//! or "stream chunk".

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Raw JSON-RPC request body (e.g. `message.sendStream` envelope).
/// Use at the A2A handler boundary instead of bare `Value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct A2aWireRequest(pub Value);

impl A2aWireRequest {
    pub fn into_inner(self) -> Value {
        self.0
    }
}

impl From<Value> for A2aWireRequest {
    fn from(v: Value) -> Self {
        Self(v)
    }
}

impl From<A2aWireRequest> for Value {
    fn from(r: A2aWireRequest) -> Self {
        r.0
    }
}

impl AsRef<Value> for A2aWireRequest {
    fn as_ref(&self) -> &Value {
        &self.0
    }
}

/// One A2A stream chunk (JSON-RPC result chunk, e.g. `{ "result": { "stream": true, "chunk": ... } }`).
/// Use for stream items instead of bare `Value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct A2aStreamChunk(pub Value);

impl A2aStreamChunk {
    pub fn into_inner(self) -> Value {
        self.0
    }
}

impl From<Value> for A2aStreamChunk {
    fn from(v: Value) -> Self {
        Self(v)
    }
}

impl From<A2aStreamChunk> for Value {
    fn from(c: A2aStreamChunk) -> Self {
        c.0
    }
}

impl AsRef<Value> for A2aStreamChunk {
    fn as_ref(&self) -> &Value {
        &self.0
    }
}
