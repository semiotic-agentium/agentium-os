//! MCP client + importer for the BAML runtime.
//!
//! Public types live in their submodules. Callers reach them via the module
//! path (`baml_tools_mcp::resolver::McpResolver`, etc.) rather than crate-root
//! re-exports so each type has exactly one canonical public path.

pub mod client;
pub mod composite;
pub mod fixture;
pub mod handler;
pub mod importer;
pub mod resolver;
pub mod runtime;
pub mod sandbox;
pub mod wire;
