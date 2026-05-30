// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Claude tool bundle for the BAML runtime.
//!
//! The host registers this crate's [`ClaudeSessionBundle`] as bundle `claude`.
//! The first tool is `claude/dev`, a session-based streaming interface to Claude.

pub mod claude_session;
pub mod metadata;
pub mod session_coordination;
pub mod spans;
pub mod tools;
pub mod user_input;

pub use claude_session::{
    AgentWorkspaceRegistry, Claude, ClaudeMessageStream, ClaudeSessionBundle, ClaudeStreamSource,
    ClaudeStreamSourceFactory, ClaudeTurnRequest,
};
pub use tools::{
    ClaudeCompletion, ClaudeEventDto, ClaudeToolNextOutput, ClaudeToolOpenInput,
    ClaudeToolSendInput, ClaudeUserContentBlockDto,
};
pub use user_input::{ContextItem, ReviewDecision, UserInput};
