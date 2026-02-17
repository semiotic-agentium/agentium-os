//! System tool bundle for the BAML runtime.
//!
//! Provides host tools that are part of the platform (e.g. system/internal_a2a for
//! agent-to-agent session calls). Agents declare these in their manifest like
//! any other tool.

pub mod a2a_session;
pub mod bundle;
mod metadata;
pub mod tools;

pub use a2a_session::A2aSessionBundle;
pub use bundle::System;
pub use tools::{
    ConversationChunk, ConversationMessage, ConversationPart, InternalA2aNextOutput,
    InternalA2aOpenInput, InternalA2aSendInput, InternalA2aTarget,
};
