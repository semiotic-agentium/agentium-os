//! Narrow trait for Mermaid diagram serving. Implemented by the runtime when
//! provenance (GraphQLite) is enabled; the API consumes this trait only.

use std::error::Error;
use std::fmt;

/// Errors from the Mermaid service (no provenance types; API stays decoupled).
#[derive(Debug)]
pub enum MermaidError {
    /// No graph found for the given context or task id.
    NotFound,
    /// Service or store unavailable (e.g. store not configured).
    Unavailable,
    /// Other error (e.g. storage failure).
    Other(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for MermaidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MermaidError::NotFound => write!(f, "no graph found for the given scope"),
            MermaidError::Unavailable => write!(f, "mermaid service unavailable"),
            MermaidError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl Error for MermaidError {}

/// Service that can produce a Mermaid diagram string for a given context or task.
/// The runtime implements this when provenance (GraphQLite) is enabled.
#[async_trait::async_trait]
pub trait MermaidService: Send + Sync {
    /// Return a Mermaid diagram (e.g. sequenceDiagram) for the given context id.
    async fn mermaid_for_context(&self, context_id: &str) -> Result<String, MermaidError>;

    /// Return a Mermaid diagram for the given task id.
    async fn mermaid_for_task(&self, task_id: &str) -> Result<String, MermaidError>;
}
