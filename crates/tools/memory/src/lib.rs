// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Memory tool bundle for the BAML runtime.
//!
//! Provides persistent graph-based cognitive memory backed by agentic-memory.
//! Agents declare `memory/*` tools in their manifest and get per-agent `.amem`
//! files at `~/.brain/{agent-name}.amem` (configurable via `BRAIN_DIR`).

pub mod bundle;
mod handlers;
pub mod manager;
pub mod metadata;
pub mod types;

pub use bundle::{Memory, MemoryBundle};
