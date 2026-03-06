//! Tool metadata registration for the internal-dev bundle.
//!
//! Registers metadata with baml-rt-tools inventory so manifest resolution
//! can find internal-dev/* tools. BamlTool impls live in test-support.

use baml_rt_tools::baml_tool;

use crate::tools::{
    A2aRelayInput, A2aRelayOutput, CalculatorInput, CalculatorOutput, DelayedInput, DelayedOutput,
    UppercaseInput, UppercaseOutput, WeatherInput, WeatherOutput,
};

#[allow(dead_code)]
#[baml_tool(
    name = "internal-dev/calculate",
    description = "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division.",
    tags = ["internal-dev", "calculate"],
    metadata_only,
    open_input = (),
    input = CalculatorInput,
    output = CalculatorOutput,
)]
pub struct InternalDevCalculate;

#[allow(dead_code)]
#[baml_tool(
    name = "internal-dev/get_weather",
    description = "Gets the current weather for a specific location. Returns temperature, condition, and humidity.",
    tags = ["internal-dev", "get_weather"],
    metadata_only,
    open_input = (),
    input = WeatherInput,
    output = WeatherOutput,
)]
pub struct InternalDevGetWeather;

#[allow(dead_code)]
#[baml_tool(
    name = "internal-dev/uppercase",
    description = "Converts a string to uppercase",
    tags = ["internal-dev", "uppercase"],
    metadata_only,
    open_input = (),
    input = UppercaseInput,
    output = UppercaseOutput,
)]
pub struct InternalDevUppercase;

#[allow(dead_code)]
#[baml_tool(
    name = "internal-dev/delayed_response",
    description = "Returns a response after a short delay (simulates async operation)",
    tags = ["internal-dev", "delayed_response"],
    metadata_only,
    open_input = (),
    input = DelayedInput,
    output = DelayedOutput,
)]
pub struct InternalDevDelayedResponse;

#[allow(dead_code)]
#[baml_tool(
    name = "internal-dev/a2a_relay",
    description = "Relays an A2A request to another in-memory agent.",
    tags = ["internal-dev", "a2a_relay"],
    metadata_only,
    open_input = (),
    input = A2aRelayInput,
    output = A2aRelayOutput,
)]
pub struct InternalDevA2aRelay;
