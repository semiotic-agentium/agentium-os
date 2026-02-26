//! Tool metadata registration for the Claude bundle.

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolHandler, parse_tool_name_and_class, register_tool,
    tools::{ToolAccess, ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};

use crate::tools::{ClaudeToolNextOutput, ClaudeToolOpenInput, ClaudeToolSendInput};

fn claude_tool_build_unused() -> Result<Arc<dyn ToolHandler>> {
    Err(BamlRtError::InvalidArgument(
        "Claude tools are registered by the host via ClaudeSessionBundle".to_string(),
    ))
}

pub fn claude_dev_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("claude/dev")
        .expect("claude/dev tool name is a compile-time constant and must parse");
    TypeBasedMetadataBuilder::<ClaudeToolOpenInput, ClaudeToolSendInput, ClaudeToolNextOutput>::new(
        name,
        class_name,
        "Host-managed Claude streaming session. Open once, send prompt/content, then call next() until completion is DONE/INTERRUPTED or INPUT_REQUIRED for resume.".to_string(),
    )
    .with_tags(vec![
        "claude".to_string(),
        "stream".to_string(),
        "session".to_string(),
    ])
    .with_access(ToolAccess::Write) // Session may write artifacts to workspace (e.g. generated files).
    .build_metadata()
}

register_tool!(claude_dev_metadata, claude_tool_build_unused);
