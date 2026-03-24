//! Test tool implementations used across test suites.

use async_trait::async_trait;
use baml_rt::{Result, tools::BamlTool};
use baml_rt_tools::bundles::{BundleType, Support};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Example weather tool
pub struct WeatherTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct WeatherInput {
    location: String,
}
impl baml_rt_tools::DescribeAction for WeatherInput {
    fn describe(&self) -> String {
        format!("checking weather for '{}'", self.location)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct WeatherOutput {
    location: String,
    temperature: String,
    temperature_f: i64,
    condition: String,
    humidity: String,
    wind_speed: String,
    description: String,
}

#[async_trait]
impl BamlTool for WeatherTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "get_weather";
    type OpenInput = ();
    type Input = WeatherInput;
    type Output = WeatherOutput;

    fn description(&self) -> &'static str {
        "Gets the current weather for a specific location. Returns temperature, condition, and humidity."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let location = args.location;

        tracing::info!(location = location.as_str(), "WeatherTool executed");

        Ok(WeatherOutput {
            location: location.clone(),
            temperature: "22°C".to_string(),
            temperature_f: 72,
            condition: "Sunny with clear skies".to_string(),
            humidity: "65%".to_string(),
            wind_speed: "10 km/h".to_string(),
            description: format!("Current weather in {}: Sunny, 22°C, 65% humidity", location),
        })
    }
}

/// Example uppercase string tool
pub struct UppercaseTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct UppercaseInput {
    text: String,
}
impl baml_rt_tools::DescribeAction for UppercaseInput {
    fn describe(&self) -> String {
        "converting text to uppercase".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct UppercaseOutput {
    result: String,
    original: String,
}

#[async_trait]
impl BamlTool for UppercaseTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "uppercase";
    type OpenInput = ();
    type Input = UppercaseInput;
    type Output = UppercaseOutput;

    fn description(&self) -> &'static str {
        "Converts a string to uppercase"
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        Ok(UppercaseOutput {
            result: args.text.to_uppercase(),
            original: args.text,
        })
    }
}

/// Delayed response tool for testing async operations
pub struct DelayedResponseTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct DelayedInput {
    message: String,
}
impl baml_rt_tools::DescribeAction for DelayedInput {
    fn describe(&self) -> String {
        "processing delayed response".to_string()
    }
}

/// Test-only bundle used by A2A error/streaming tests.
pub struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for A2A tests"
    }
}

/// Example add-numbers tool for A2A tests.
pub struct AddNumbersTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct AddNumbersInput {
    pub a: f64,
    pub b: f64,
}
impl baml_rt_tools::DescribeAction for AddNumbersInput {
    fn describe(&self) -> String {
        format!("adding {} + {}", self.a, self.b)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct AddNumbersOutput {
    pub result: f64,
}

#[async_trait]
impl BamlTool for AddNumbersTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "add_numbers";
    type OpenInput = ();
    type Input = AddNumbersInput;
    type Output = AddNumbersOutput;

    fn description(&self) -> &'static str {
        "Adds two numbers"
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        Ok(AddNumbersOutput {
            result: args.a + args.b,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct DelayedOutput {
    response: String,
    timestamp: String,
}

#[async_trait]
impl BamlTool for DelayedResponseTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "delayed_response";
    type OpenInput = ();
    type Input = DelayedInput;
    type Output = DelayedOutput;

    fn description(&self) -> &'static str {
        "Returns a response after a short delay (simulates async operation)"
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        use tokio::time::{Duration, sleep};

        // Simulate async work
        sleep(Duration::from_millis(50)).await;

        Ok(DelayedOutput {
            response: format!("Delayed: {}", args.message),
            timestamp: format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            ),
        })
    }
}
