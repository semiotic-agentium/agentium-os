//! Tool metadata registration for the internal-dev bundle (single mechanism: register_tool!).
//!
//! Registers metadata with baml-rt-tools inventory so manifest resolution
//! can find internal-dev/* tools. BamlTool impls live in test-support.

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::tools::{ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder};
use baml_rt_tools::{ToolHandler, parse_tool_name_and_class, register_tool};
use std::sync::Arc;

fn internal_dev_build_unused() -> Result<Arc<dyn ToolHandler>> {
    Err(BamlRtError::InvalidArgument(
        "internal-dev tools are provided by test-support at runtime".to_string(),
    ))
}

use crate::tools::{
    A2aRelayInput, A2aRelayOutput, CalculatorInput, CalculatorOutput, DelayedInput, DelayedOutput,
    UppercaseInput, UppercaseOutput, WeatherInput, WeatherOutput,
};

fn internal_dev_calculate_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("internal-dev/calculate")
        .expect("internal-dev/calculate is a compile-time constant");
    TypeBasedMetadataBuilder::<(), CalculatorInput, CalculatorOutput>::new(
        name.clone(),
        class_name,
        "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division.".to_string(),
    )
    .with_tags(vec!["internal-dev".to_string(), "calculate".to_string()])
    .build_metadata()
}

fn internal_dev_get_weather_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("internal-dev/get_weather")
        .expect("internal-dev/get_weather is a compile-time constant");
    TypeBasedMetadataBuilder::<(), WeatherInput, WeatherOutput>::new(
        name.clone(),
        class_name,
        "Gets the current weather for a specific location. Returns temperature, condition, and humidity.".to_string(),
    )
    .with_tags(vec!["internal-dev".to_string(), "get_weather".to_string()])
    .build_metadata()
}

fn internal_dev_uppercase_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("internal-dev/uppercase")
        .expect("internal-dev/uppercase is a compile-time constant");
    TypeBasedMetadataBuilder::<(), UppercaseInput, UppercaseOutput>::new(
        name.clone(),
        class_name,
        "Converts a string to uppercase".to_string(),
    )
    .with_tags(vec!["internal-dev".to_string(), "uppercase".to_string()])
    .build_metadata()
}

fn internal_dev_delayed_response_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("internal-dev/delayed_response")
        .expect("internal-dev/delayed_response is a compile-time constant");
    TypeBasedMetadataBuilder::<(), DelayedInput, DelayedOutput>::new(
        name.clone(),
        class_name,
        "Returns a response after a short delay (simulates async operation)".to_string(),
    )
    .with_tags(vec![
        "internal-dev".to_string(),
        "delayed_response".to_string(),
    ])
    .build_metadata()
}

fn internal_dev_a2a_relay_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("internal-dev/a2a_relay")
        .expect("internal-dev/a2a_relay is a compile-time constant");
    TypeBasedMetadataBuilder::<(), A2aRelayInput, A2aRelayOutput>::new(
        name.clone(),
        class_name,
        "Relays an A2A request to another in-memory agent.".to_string(),
    )
    .with_tags(vec!["internal-dev".to_string(), "a2a_relay".to_string()])
    .build_metadata()
}

register_tool!(internal_dev_calculate_metadata, internal_dev_build_unused);
register_tool!(internal_dev_get_weather_metadata, internal_dev_build_unused);
register_tool!(internal_dev_uppercase_metadata, internal_dev_build_unused);
register_tool!(
    internal_dev_delayed_response_metadata,
    internal_dev_build_unused
);
register_tool!(internal_dev_a2a_relay_metadata, internal_dev_build_unused);
