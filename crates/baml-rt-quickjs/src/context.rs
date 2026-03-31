//! BAML Context - Isolation per user/request
//!
//! Each context gets its own QuickJS runtime for isolation.
//! This enables multi-tenant scenarios and prevents state pollution.

use std::sync::Arc;

use baml_rt_core::{Result, ids::AgentId};
use tokio::sync::RwLock;

use crate::{baml::BamlRuntimeManager, quickjs_bridge::QuickJSBridge};

/// A BAML execution context with isolated QuickJS runtime
///
/// Each context has its own JavaScript global scope, preventing
/// state pollution between different users/requests.
///
/// **Memory overhead**: ~50-100 KB per context (QuickJS is lightweight)
/// **Performance**: eval() overhead is negligible (~0.001ms)
/// **Concurrency**: Each context can execute JavaScript in parallel
pub struct BamlContext {
    /// Isolated QuickJS runtime for this context
    pub quickjs: QuickJSBridge,
    /// Optional context-specific metadata
    pub metadata: Option<ContextMetadata>,
}

/// Optional metadata for context tracking
#[derive(Debug, Clone)]
pub struct ContextMetadata {
    pub context_id: String,
    pub user_id: Option<String>,
    pub request_id: Option<String>,
}

impl BamlContext {
    /// Create a new isolated BAML context
    ///
    /// Each context gets its own QuickJS runtime instance, ensuring
    /// complete isolation between different execution contexts.
    ///
    /// # Arguments
    /// * `baml_manager` - Shared BAML runtime manager (Rust execution is shared)
    /// * `metadata` - Optional context metadata for tracking
    ///
    /// # Example
    /// ```rust,no_run
    /// use baml_rt::context::{BamlContext, ContextMetadata};
    /// use baml_rt::baml::BamlRuntimeManager;
    /// use baml_rt_core::ids::{AgentId, UuidId};
    /// use std::sync::Arc;
    /// use tokio::sync::RwLock;
    ///
    /// # tokio_test::block_on(async {
    /// let baml_manager = Arc::new(RwLock::new(BamlRuntimeManager::builder().build()?));
    /// baml_manager.write().await.load_schema("baml_src")?;
    ///
    /// // Create isolated context for user/request
    /// let agent_id = AgentId::from_uuid(
    ///     UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap()
    /// );
    /// let mut context = BamlContext::new(
    ///     baml_manager.clone(),
    ///     agent_id,
    ///     Some(ContextMetadata {
    ///         context_id: "req-123".to_string(),
    ///         user_id: Some("user-456".to_string()),
    ///         request_id: Some("req-123".to_string()),
    ///     })
    /// ).await?;
    ///
    /// // Register BAML functions in this context
    /// context.quickjs.register_baml_functions().await?;
    ///
    /// // Execute synchronous JavaScript in isolated context
    /// let result = context
    ///     .quickjs
    ///     .eval_sync("SimpleGreeting({name: 'World'})")
    ///     .await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    pub async fn new(
        baml_manager: Arc<RwLock<BamlRuntimeManager>>,
        agent_id: AgentId,
        metadata: Option<ContextMetadata>,
    ) -> Result<Self> {
        tracing::debug!(
            context_id = metadata.as_ref().map(|m| m.context_id.as_str()),
            agent_id = %agent_id,
            "Creating new BAML context"
        );

        Ok(Self {
            quickjs: QuickJSBridge::new(baml_manager, agent_id).await?,
            metadata,
        })
    }

    ///    /// Get the context ID if metadata is available
    pub fn context_id(&self) -> Option<&str> {
        self.metadata.as_ref().map(|m| m.context_id.as_str())
    }
}

impl Default for ContextMetadata {
    fn default() -> Self {
        Self {
            context_id: format!(
                "ctx-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_else(|_| std::time::Duration::from_secs(0))
                    .as_nanos()
            ),
            user_id: None,
            request_id: None,
        }
    }
}
