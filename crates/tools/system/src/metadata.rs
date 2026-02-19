//! Tool metadata registration for the system bundle.

use baml_rt_tools::{
    parse_tool_name_and_class, register_tool_metadata,
    tools::{ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};

use crate::tools::{InternalA2aNextOutput, InternalA2aOpenInput, InternalA2aSendInput};

fn build_a2a_metadata(tool_name: &str) -> ToolFunctionMetadata {
    let (name, class_name) =
        parse_tool_name_and_class(tool_name).expect("a2a tool name is a compile-time constant");
    TypeBasedMetadataBuilder::<InternalA2aOpenInput, InternalA2aSendInput, InternalA2aNextOutput>::new(
        name,
        class_name,
        "Opens a session to another agent by route key. Send structured parts or text; next() returns batched id-free chunks.".to_string(),
    )
    .with_tags(vec!["system".to_string(), "a2a".to_string()])
    .build_metadata()
}

pub fn system_internal_a2a_metadata() -> ToolFunctionMetadata {
    build_a2a_metadata("system/internal_a2a")
}

register_tool_metadata!(system_internal_a2a_metadata);
