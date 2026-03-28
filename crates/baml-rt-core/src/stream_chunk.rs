//! Shared ownership for JSON stream payloads (`Arc<serde_json::Value>`).
//!
//! Used on the QuickJS yield → A2A router path so multiple stages can reference the same
//! chunk without deep [`serde_json::Value`] clones. Stages that need an owned tree call
//! [`SharedChunk::into_value`] — when this is the last strong reference, the allocation is
//! moved out without cloning.

use std::{ops::Deref, sync::Arc};

use serde_json::Value;

/// Arc-backed JSON body for in-process stream chunks (JS yield / relay before wire normalize).
#[derive(Debug, Clone)]
pub struct SharedChunk(Arc<Value>);

impl SharedChunk {
    #[must_use]
    pub fn new(value: Value) -> Self {
        Self(Arc::new(value))
    }

    /// Extract the JSON tree. If this is the last reference, the allocation is moved out without cloning.
    pub fn into_value(self) -> Value {
        Arc::unwrap_or_clone(self.0)
    }
}

impl Deref for SharedChunk {
    type Target = Value;

    fn deref(&self) -> &Value {
        &self.0
    }
}

impl AsRef<Value> for SharedChunk {
    fn as_ref(&self) -> &Value {
        &self.0
    }
}

impl From<Value> for SharedChunk {
    fn from(value: Value) -> Self {
        Self::new(value)
    }
}

impl PartialEq<Value> for SharedChunk {
    fn eq(&self, other: &Value) -> bool {
        self.as_ref() == other
    }
}

/// Borrow JSON for routing, state checks, and cheap tracing without forcing a deep clone.
///
/// Normalization and wire code that must own the tree consume via [`SharedChunk::into_value`]
/// or clone from [`StreamChunkBody::as_json`].
pub trait StreamChunkBody {
    fn as_json(&self) -> &Value;
}

impl StreamChunkBody for SharedChunk {
    fn as_json(&self) -> &Value {
        self
    }
}

impl StreamChunkBody for Value {
    fn as_json(&self) -> &Value {
        self
    }
}
