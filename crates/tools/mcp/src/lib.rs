//! MCP client + importer for the BAML runtime.

pub mod client;
pub mod composite;
pub mod fixture;
pub mod handler;
pub mod importer;
pub mod resolver;
pub mod runtime;
pub mod sandbox;
pub mod wire;

pub use client::{CLIENT_PROTOCOL_VERSION, McpRpcError, McpStdioClient};
pub use composite::CompositeResolver;
pub use handler::McpToolHandler;
pub use importer::{EnvSecretResolver, ImportError, ImportOptions, Importer, SecretResolver};
pub use resolver::{McpResolver, default_mcp_resolver};
pub use runtime::{ConnectionError, McpConnection, ServerLaunch};
pub use sandbox::{SandboxError, SandboxedChild, SpawnSpec};
