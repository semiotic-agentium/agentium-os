// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tool metadata registration for the internal-dev bundle.
//!
//! Registers metadata with baml-rt-tools inventory so manifest resolution
//! can find internal-dev/* tools. BamlTool impls live in test-support.

use baml_rt_tools::baml_tool;

use crate::tools::{
    A2aRelayInput, A2aRelayOutput, CalculatorInput, CalculatorOutput, DelayedInput, DelayedOutput,
    UppercaseInput, UppercaseOutput, WeatherInput, WeatherOutput,
};

#[expect(
    dead_code,
    reason = "type exists only to register tool metadata via the baml_tool macro; it is never instantiated directly"
)]
#[baml_tool(
    name = "internal-dev/calculate",
    description = "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division.",
    tags = ["internal-dev", "calculate"],
    access = Read,
    metadata_only,
    open_input = (),
    input = CalculatorInput,
    output = CalculatorOutput,
)]
pub struct InternalDevCalculate;

#[expect(
    dead_code,
    reason = "type exists only to register tool metadata via the baml_tool macro; it is never instantiated directly"
)]
#[baml_tool(
    name = "internal-dev/get_weather",
    description = "Gets the current weather for a specific location. Returns temperature, condition, and humidity.",
    tags = ["internal-dev", "get_weather"],
    access = Read,
    event_sources = ["weather"],
    metadata_only,
    open_input = (),
    input = WeatherInput,
    output = WeatherOutput,
)]
pub struct InternalDevGetWeather;

#[expect(
    dead_code,
    reason = "type exists only to register tool metadata via the baml_tool macro; it is never instantiated directly"
)]
#[baml_tool(
    name = "internal-dev/uppercase",
    description = "Converts a string to uppercase",
    tags = ["internal-dev", "uppercase"],
    access = Read,
    metadata_only,
    open_input = (),
    input = UppercaseInput,
    output = UppercaseOutput,
)]
pub struct InternalDevUppercase;

#[expect(
    dead_code,
    reason = "type exists only to register tool metadata via the baml_tool macro; it is never instantiated directly"
)]
#[baml_tool(
    name = "internal-dev/delayed_response",
    description = "Returns a response after a short delay (simulates async operation)",
    tags = ["internal-dev", "delayed_response"],
    access = Read,
    metadata_only,
    open_input = (),
    input = DelayedInput,
    output = DelayedOutput,
)]
pub struct InternalDevDelayedResponse;

#[expect(
    dead_code,
    reason = "type exists only to register tool metadata via the baml_tool macro; it is never instantiated directly"
)]
#[baml_tool(
    name = "internal-dev/a2a_relay",
    description = "Relays an A2A request to another in-memory agent.",
    tags = ["internal-dev", "a2a_relay"],
    access = Write,
    metadata_only,
    open_input = (),
    input = A2aRelayInput,
    output = A2aRelayOutput,
)]
pub struct InternalDevA2aRelay;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_tool_declares_event_sources() {
        let meta = internal_dev_get_weather_metadata();
        assert_eq!(meta.event_sources.len(), 1);
        assert_eq!(meta.event_sources[0].as_str(), "weather");
    }

    #[test]
    fn calculate_tool_has_no_event_sources() {
        let meta = internal_dev_calculate_metadata();
        assert!(meta.event_sources.is_empty());
    }
}
