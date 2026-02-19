//! Tool metadata registration for the internal-dev bundle.
//!
//! Registers metadata with baml-rt-tools inventory so manifest resolution
//! can find internal-dev/* tools.

use baml_rt_tools::{
    parse_tool_name_and_class, register_tool_metadata,
    tools::{ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};

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

register_tool_metadata!(internal_dev_calculate_metadata);
register_tool_metadata!(internal_dev_get_weather_metadata);
register_tool_metadata!(internal_dev_uppercase_metadata);
register_tool_metadata!(internal_dev_delayed_response_metadata);
register_tool_metadata!(internal_dev_a2a_relay_metadata);
