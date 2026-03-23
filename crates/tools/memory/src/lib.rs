//! Memory tool bundle for the BAML runtime.
//!
//! Provides persistent graph-based cognitive memory backed by agentic-memory.
//! Agents declare `memory/*` tools in their manifest and get per-agent `.amem`
//! files at `~/.brain/{agent-name}.amem` (configurable via `BRAIN_DIR`).
//!
//! Also provides `memory/context_memory_resolve` for shared context retrieval
//! from provenance when provenance support is enabled.

pub mod bundle;
pub mod context_memory_resolve;
mod handlers;
pub mod manager;
pub mod metadata;
pub mod types;

pub use bundle::{Memory, MemoryBundle};
pub use context_memory_resolve::{ContextMemoryResolveTool, context_memory_resolve_handler};
