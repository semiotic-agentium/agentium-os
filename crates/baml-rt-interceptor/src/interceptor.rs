//! Interceptor system for LLM and tool call governance
//!
//! Provides a trait-based system for intercepting, logging, and potentially blocking
//! LLM calls and tool executions for governance, tracing, and security purposes.

use async_trait::async_trait;
use baml_rt_core::ids::ContextId;
use baml_rt_core::{BamlRtError, Result};
use serde_json::Value;
use std::sync::Arc;

/// Result of an interception decision
#[derive(Debug, Clone)]
pub enum InterceptorDecision {
    /// Allow the call to proceed
    Allow,

    /// Block the call with this error message
    /// The error will be wrapped in a ToolExecution or BamlRuntime error
    Block(String),
}

/// Context information about an LLM call
#[derive(Debug, Clone)]
pub struct LLMCallContext {
    /// The client/provider name (e.g., "openai", "anthropic")
    pub client: String,

    /// The model name
    pub model: String,

    /// The function name that triggered this LLM call
    pub function_name: String,

    /// The active context ID for this call
    pub context_id: ContextId,

    /// The prompt/messages being sent
    pub prompt: Value,

    /// Additional metadata
    pub metadata: Value,
}

/// Context information about a tool call
#[derive(Debug, Clone)]
pub struct ToolCallContext {
    /// The tool name
    pub tool_name: String,

    /// The function name that triggered this tool call (if applicable)
    pub function_name: Option<String>,

    /// The tool arguments
    pub args: Value,

    /// The active context ID for this call
    pub context_id: ContextId,

    /// Additional metadata
    pub metadata: Value,
}

/// Trait for intercepting LLM calls
#[async_trait]
pub trait LLMInterceptor: Send + Sync + 'static {
    /// Intercept an LLM call before execution
    ///
    /// # Arguments
    /// * `context` - Information about the LLM call
    ///
    /// # Returns
    /// A decision on whether to allow or block the call
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision>;

    /// Called after an LLM call completes (regardless of success/failure)
    ///
    /// # Arguments
    /// * `context` - The original call context
    /// * `result` - The result of the LLM call (Ok if successful, Err if failed)
    /// * `duration_ms` - How long the call took in milliseconds
    async fn on_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    );
}

/// Trait for intercepting tool calls
#[async_trait]
pub trait ToolInterceptor: Send + Sync + 'static {
    /// Intercept a tool call before execution
    ///
    /// # Arguments
    /// * `context` - Information about the tool call
    ///
    /// # Returns
    /// A decision on whether to allow or block the call
    async fn intercept_tool_call(&self, context: &ToolCallContext) -> Result<InterceptorDecision>;

    /// Called after a tool call completes (regardless of success/failure)
    ///
    /// # Arguments
    /// * `context` - The original call context
    /// * `result` - The result of the tool call (Ok if successful, Err if failed)
    /// * `duration_ms` - How long the call took in milliseconds
    async fn on_tool_call_complete(
        &self,
        context: &ToolCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    );
}

/// Pipeline for composing multiple interceptors
///
/// This allows interceptors to be chained together in a pipeline pattern.
/// Interceptors are executed in order, and if any interceptor blocks,
/// subsequent interceptors are not called.
pub struct InterceptorPipeline<I: ?Sized> {
    pub(crate) interceptors: Vec<Arc<I>>,
}

impl<I: ?Sized> InterceptorPipeline<I> {
    /// Create a new empty pipeline
    pub fn new() -> Self {
        Self {
            interceptors: Vec::new(),
        }
    }

    /// Add an interceptor to the pipeline
    ///
    /// Interceptors are executed in the order they are added.
    pub fn with_interceptor(mut self, interceptor: Arc<I>) -> Self {
        self.interceptors.push(interceptor);
        self
    }

    /// Add multiple interceptors to the pipeline
    pub fn add_all(mut self, interceptors: Vec<Arc<I>>) -> Self {
        self.interceptors.extend(interceptors);
        self
    }

    /// Get all interceptors in the pipeline
    pub fn interceptors(&self) -> &[Arc<I>] {
        &self.interceptors
    }

    /// Get the number of interceptors in the pipeline
    pub fn len(&self) -> usize {
        self.interceptors.len()
    }

    /// Check if the pipeline is empty
    pub fn is_empty(&self) -> bool {
        self.interceptors.is_empty()
    }
}

impl<I: ?Sized> Default for InterceptorPipeline<I> {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry for managing interceptors
///
/// This registry manages pipelines of interceptors for both LLM and tool calls.
pub struct InterceptorRegistry {
    pub(crate) llm_pipeline: InterceptorPipeline<dyn LLMInterceptor>,
    pub(crate) tool_pipeline: InterceptorPipeline<dyn ToolInterceptor>,
}

impl InterceptorRegistry {
    /// Create a new empty interceptor registry
    pub fn new() -> Self {
        Self {
            llm_pipeline: InterceptorPipeline::new(),
            tool_pipeline: InterceptorPipeline::new(),
        }
    }

    /// Create a new registry from pipelines
    pub fn from_pipelines(
        llm_pipeline: InterceptorPipeline<dyn LLMInterceptor>,
        tool_pipeline: InterceptorPipeline<dyn ToolInterceptor>,
    ) -> Self {
        Self {
            llm_pipeline,
            tool_pipeline,
        }
    }

    /// Register an LLM interceptor
    ///
    /// Interceptors are called in registration order. If any interceptor
    /// blocks the call, subsequent interceptors are not called.
    pub fn register_llm_interceptor<I: LLMInterceptor>(&mut self, interceptor: I) {
        let pipeline = std::mem::take(&mut self.llm_pipeline);
        self.llm_pipeline =
            pipeline.with_interceptor(Arc::new(interceptor) as Arc<dyn LLMInterceptor>);
    }

    /// Register a tool interceptor
    ///
    /// Interceptors are called in registration order. If any interceptor
    /// blocks the call, subsequent interceptors are not called.
    pub fn register_tool_interceptor<I: ToolInterceptor>(&mut self, interceptor: I) {
        let pipeline = std::mem::take(&mut self.tool_pipeline);
        self.tool_pipeline =
            pipeline.with_interceptor(Arc::new(interceptor) as Arc<dyn ToolInterceptor>);
    }

    /// Add an LLM interceptor pipeline
    ///
    /// This allows composing multiple interceptors into a pipeline.
    pub fn with_llm_pipeline(mut self, pipeline: InterceptorPipeline<dyn LLMInterceptor>) -> Self {
        // Merge the new pipeline with existing interceptors
        let existing = std::mem::take(&mut self.llm_pipeline);
        let mut merged = InterceptorPipeline::new();

        // Add existing interceptors
        for interceptor in existing.interceptors() {
            merged.interceptors.push(interceptor.clone());
        }

        // Add new pipeline interceptors
        for interceptor in pipeline.interceptors() {
            merged.interceptors.push(interceptor.clone());
        }

        self.llm_pipeline = merged;
        self
    }

    /// Add a tool interceptor pipeline
    ///
    /// This allows composing multiple interceptors into a pipeline.
    pub fn with_tool_pipeline(
        mut self,
        pipeline: InterceptorPipeline<dyn ToolInterceptor>,
    ) -> Self {
        // Merge the new pipeline with existing interceptors
        let existing = std::mem::take(&mut self.tool_pipeline);
        let mut merged = InterceptorPipeline::new();

        // Add existing interceptors
        for interceptor in existing.interceptors() {
            merged.interceptors.push(interceptor.clone());
        }

        // Add new pipeline interceptors
        for interceptor in pipeline.interceptors() {
            merged.interceptors.push(interceptor.clone());
        }

        self.tool_pipeline = merged;
        self
    }

    /// Merge an LLM interceptor pipeline into the registry.
    ///
    /// This preserves existing interceptors and appends the provided pipeline.
    pub fn merge_llm_pipeline(&mut self, pipeline: InterceptorPipeline<dyn LLMInterceptor>) {
        let existing = std::mem::take(&mut self.llm_pipeline);
        let mut merged = InterceptorPipeline::new();
        merged.interceptors.extend(existing.interceptors);
        merged.interceptors.extend(pipeline.interceptors);
        self.llm_pipeline = merged;
    }

    /// Merge a tool interceptor pipeline into the registry.
    ///
    /// This preserves existing interceptors and appends the provided pipeline.
    pub fn merge_tool_pipeline(&mut self, pipeline: InterceptorPipeline<dyn ToolInterceptor>) {
        let existing = std::mem::take(&mut self.tool_pipeline);
        let mut merged = InterceptorPipeline::new();
        merged.interceptors.extend(existing.interceptors);
        merged.interceptors.extend(pipeline.interceptors);
        self.tool_pipeline = merged;
    }

    /// Execute LLM interceptors and return the final decision
    ///
    /// Returns Ok(Allow) if all interceptors allow, or Err if any block
    pub async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> Result<InterceptorDecision> {
        for interceptor in self.llm_pipeline.interceptors() {
            match interceptor.intercept_llm_call(context).await {
                Ok(InterceptorDecision::Allow) => {
                    // Continue to next interceptor
                }
                Ok(InterceptorDecision::Block(msg)) => {
                    return Err(BamlRtError::BamlRuntime(format!(
                        "LLM call blocked by interceptor: {}",
                        msg
                    )));
                }
                Err(e) => {
                    // Interceptor itself failed - log but continue?
                    tracing::warn!(error = ?e, "LLM interceptor failed");
                }
            }
        }

        Ok(InterceptorDecision::Allow)
    }

    /// Execute tool interceptors and return the final decision
    ///
    /// Returns Ok(Allow) if all interceptors allow, or Err if any block
    pub async fn intercept_tool_call(
        &self,
        context: &ToolCallContext,
    ) -> Result<InterceptorDecision> {
        for interceptor in self.tool_pipeline.interceptors() {
            match interceptor.intercept_tool_call(context).await {
                Ok(InterceptorDecision::Allow) => {
                    // Continue to next interceptor
                }
                Ok(InterceptorDecision::Block(msg)) => {
                    return Err(BamlRtError::ToolExecution(format!(
                        "Tool call blocked by interceptor: {}",
                        msg
                    )));
                }
                Err(e) => {
                    // Interceptor itself failed - log but continue?
                    tracing::warn!("Tool interceptor failed: {}", e);
                }
            }
        }

        Ok(InterceptorDecision::Allow)
    }

    /// Notify all LLM interceptors of a completed call
    pub async fn notify_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        for interceptor in self.llm_pipeline.interceptors() {
            interceptor
                .on_llm_call_complete(context, result, duration_ms)
                .await;
        }
    }

    /// Notify all tool interceptors of a completed call
    pub async fn notify_tool_call_complete(
        &self,
        context: &ToolCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        for interceptor in self.tool_pipeline.interceptors() {
            interceptor
                .on_tool_call_complete(context, result, duration_ms)
                .await;
        }
    }

    /// Get the LLM interceptor pipeline (for inspection)
    pub fn llm_pipeline(&self) -> &InterceptorPipeline<dyn LLMInterceptor> {
        &self.llm_pipeline
    }

    /// Get the tool interceptor pipeline (for inspection)
    pub fn tool_pipeline(&self) -> &InterceptorPipeline<dyn ToolInterceptor> {
        &self.tool_pipeline
    }

    /// Get all LLM interceptors (for inspection)
    pub fn llm_interceptors(&self) -> &[Arc<dyn LLMInterceptor>] {
        self.llm_pipeline.interceptors()
    }

    /// Get all tool interceptors (for inspection)
    pub fn tool_interceptors(&self) -> &[Arc<dyn ToolInterceptor>] {
        self.tool_pipeline.interceptors()
    }
}

impl Default for InterceptorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
