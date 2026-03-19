//! Tracing interceptor for LLM and tool calls
//!
//! This module provides interceptors that emit structured tracing events
//! for all LLM and tool calls, enabling observability and debugging.
//!
//! These spans are children of invoke_function/invoke_baml_function spans and inherit
//! context automatically through OTEL span nesting - no need to repeat attributes.

use async_trait::async_trait;
use baml_rt_core::Result;
use serde_json::Value;
use tracing::{Level, error, info, span};

use crate::interceptor::{
    InterceptorDecision, LLMCallContext, LLMInterceptor, ToolCallContext, ToolInterceptor,
};

/// Tracing interceptor for LLM calls
///
/// This interceptor emits structured tracing events for all LLM calls,
/// including pre-execution (intercept_llm_call) and post-execution (on_llm_call_complete).
pub struct TracingLLMInterceptor;

impl TracingLLMInterceptor {
    /// Create a new tracing interceptor for LLM calls
    pub fn new() -> Self {
        Self
    }
}

impl Default for TracingLLMInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LLMInterceptor for TracingLLMInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        // Create a child span for this LLM call - inherits context from parent automatically
        let span = span!(
            Level::DEBUG,
            "baml_rt.llm_call",
            client = %context.client,
            model = %context.model,
            function = %context.function_id.prompt_name(),
            context_id = %context.runtime_scope.context_id(),
        );
        let _guard = span.enter();

        info!(
            prompt = ?context.prompt,
            metadata = ?context.metadata,
            "LLM call intercepted (pre-execution)"
        );

        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        // Create a child span - inherits context from parent automatically
        let span = span!(
            Level::DEBUG,
            "baml_rt.llm_call_complete",
            client = %context.client,
            model = %context.model,
            function = %context.function_id.prompt_name(),
            duration_ms = duration_ms,
            context_id = %context.runtime_scope.context_id(),
        );
        let _guard = span.enter();

        match result {
            Ok(value) => {
                info!(
                    result = ?value,
                    success = true,
                    "LLM call completed"
                );
            }
            Err(e) => {
                error!(
                    error = %e,
                    success = false,
                    "LLM call failed"
                );
            }
        }
    }
}

/// Tracing interceptor for tool calls
///
/// This interceptor emits structured tracing events for all tool calls,
/// including pre-execution (intercept_tool_call) and post-execution (on_tool_call_complete).
pub struct TracingToolInterceptor;

impl TracingToolInterceptor {
    /// Create a new tracing interceptor for tool calls
    pub fn new() -> Self {
        Self
    }
}

impl Default for TracingToolInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolInterceptor for TracingToolInterceptor {
    async fn intercept_tool_call(&self, context: &ToolCallContext) -> Result<InterceptorDecision> {
        // Create a child span for this tool call - inherits context from parent automatically
        let span = span!(
            Level::DEBUG,
            "baml_rt.tool_call",
            tool = %context.tool_name,
            function = ?context.function_name,
            context_id = %context.runtime_scope.context_id(),
        );
        let _guard = span.enter();

        // Use structured fields - no string interpolation in log messages
        info!(
            args = ?context.args,
            metadata = ?context.metadata,
            "Tool call intercepted"
        );

        Ok(InterceptorDecision::Allow)
    }

    async fn on_tool_call_complete(
        &self,
        context: &ToolCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        // Create a child span - inherits context from parent automatically
        let span = span!(
            Level::DEBUG,
            "baml_rt.tool_call_complete",
            tool = %context.tool_name,
            function = ?context.function_name,
            duration_ms = duration_ms,
            context_id = %context.runtime_scope.context_id(),
        );
        let _guard = span.enter();

        match result {
            Ok(value) => {
                info!(
                    result = ?value,
                    success = true,
                    "Tool call completed"
                );
            }
            Err(e) => {
                error!(
                    error = %e,
                    success = false,
                    "Tool call failed"
                );
            }
        }
    }
}

/// Combined tracing interceptor for both LLM and tool calls
///
/// This is a convenience type that implements both `LLMInterceptor` and `ToolInterceptor`,
/// making it easy to add tracing to both types of calls with a single interceptor.
pub struct TracingInterceptor {
    llm: TracingLLMInterceptor,
    tool: TracingToolInterceptor,
}

impl TracingInterceptor {
    /// Create a new combined tracing interceptor
    pub fn new() -> Self {
        Self {
            llm: TracingLLMInterceptor::new(),
            tool: TracingToolInterceptor::new(),
        }
    }
}

impl Default for TracingInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

// Delegate LLMInterceptor implementation
#[async_trait]
impl LLMInterceptor for TracingInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        self.llm.intercept_llm_call(context).await
    }

    async fn on_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        self.llm
            .on_llm_call_complete(context, result, duration_ms)
            .await;
    }
}

// Delegate ToolInterceptor implementation
#[async_trait]
impl ToolInterceptor for TracingInterceptor {
    async fn intercept_tool_call(&self, context: &ToolCallContext) -> Result<InterceptorDecision> {
        self.tool.intercept_tool_call(context).await
    }

    async fn on_tool_call_complete(
        &self,
        context: &ToolCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        self.tool
            .on_tool_call_complete(context, result, duration_ms)
            .await;
    }
}
