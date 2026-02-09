//! Trait definitions for runtime components
//!
//! This module provides trait-based abstractions for runtime operations,
//! enabling better testability and flexibility.
//!
//! These traits define the core interfaces for:
//! - Executing BAML functions
//! - Loading BAML schemas
//! - Managing tool registries

use crate::baml::BamlRuntimeManager;
use crate::quickjs_bridge::QuickJSBridge;
use async_trait::async_trait;
use baml_rt_core::Result;
use baml_rt_tools::{BamlTool, ToolSessionId, ToolStep};
use serde_json::Value;

/// Trait for executing BAML functions
#[async_trait]
pub trait BamlFunctionExecutor: Send + Sync {
    /// Execute a BAML function by name with given arguments
    async fn execute_function(&self, function_name: &str, args: Value) -> Result<Value>;

    /// List all available function names
    fn list_functions(&self) -> Vec<String>;
}

/// Trait for schema loading operations
pub trait SchemaLoader: Send + Sync {
    /// Load a BAML schema from the given path
    fn load_schema(&mut self, schema_path: &str) -> Result<()>;

    /// Check if a schema is loaded
    fn is_schema_loaded(&self) -> bool;
}

/// Trait for tool registry operations
#[async_trait]
pub trait ToolRegistryTrait: Send + Sync {
    /// Register a tool
    async fn register_tool<T: BamlTool>(&mut self, tool: T) -> Result<()>;

    /// Execute a tool by name
    async fn execute_tool(&self, name: &str, args: Value) -> Result<Value>;

    /// List all registered tool names
    async fn list_tools(&self) -> Vec<String>;

    async fn open_tool_session(
        &self,
        tool_name: &str,
        open_input: serde_json::Value,
    ) -> Result<ToolSessionId>;
    async fn tool_session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()>;
    async fn tool_session_next(&self, session_id: &ToolSessionId) -> Result<ToolStep>;
    async fn tool_session_finish(&self, session_id: &ToolSessionId) -> Result<()>;
    async fn tool_session_abort(
        &self,
        session_id: &ToolSessionId,
        reason: Option<String>,
    ) -> Result<()>;
}

/// Trait for hosting JavaScript runtime evaluation.
#[async_trait(?Send)]
pub trait JsRuntimeHost: Send + Sync {
    async fn eval_json(&mut self, code: &str) -> Result<Value>;
}

/// Trait for invoking BAML functions and tools.
#[async_trait(?Send)]
pub trait BamlGateway: Send + Sync {
    async fn invoke_baml_function(&self, function_name: &str, args: Value) -> Result<Value>;
    async fn execute_tool_from_baml_result(&self, baml_result: Value) -> Result<Value>;
}

#[async_trait(?Send)]
impl JsRuntimeHost for QuickJSBridge {
    async fn eval_json(&mut self, code: &str) -> Result<Value> {
        self.evaluate(None, code).await
    }
}

#[async_trait(?Send)]
impl BamlGateway for BamlRuntimeManager {
    async fn invoke_baml_function(&self, function_name: &str, args: Value) -> Result<Value> {
        self.invoke_function(function_name, args).await
    }

    async fn execute_tool_from_baml_result(&self, baml_result: Value) -> Result<Value> {
        self.execute_tool_from_baml_result(baml_result).await
    }
}
