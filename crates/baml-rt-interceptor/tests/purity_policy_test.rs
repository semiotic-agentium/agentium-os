//! Compile-time policy tests for typed interceptor metadata.

use baml_rt_core::context::RuntimeScope;
use baml_rt_interceptor::{LLMCallContext, ToolCallContext};
use serde_json::Value;

fn assert_llm_context_contract(context: &LLMCallContext) {
    let _scope: &RuntimeScope = &context.runtime_scope;
    let _metadata: &Value = &context.metadata;
}

fn assert_tool_context_contract(context: &ToolCallContext) {
    let _scope: &RuntimeScope = &context.runtime_scope;
    let _metadata: &Value = &context.metadata;
}

#[test]
fn interceptor_context_metadata_is_typed() {
    let _ = assert_llm_context_contract as fn(&LLMCallContext);
    let _ = assert_tool_context_contract as fn(&ToolCallContext);
}
