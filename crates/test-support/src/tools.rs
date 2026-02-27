//! BamlTool implementations for the internal-dev bundle (testing only).
//!
//! Lives in test-support to avoid a dependency cycle: internal-dev has no baml-rt dependency
//! so baml-rt-builder can depend on it; tool impls need baml-rt so they live here.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt::{Result, tools::BamlTool};
use baml_rt_tools_internal_dev::{
    A2aRelayInput, A2aRelayOutput, CalculatorInput, CalculatorOutput, DelayedInput, DelayedOutput,
    InternalDev, MathOperation, UppercaseInput, UppercaseOutput, WeatherInput, WeatherOutput,
};
use serde_json::Value;
use tokio::task;

pub struct WeatherTool;

#[async_trait]
impl BamlTool for WeatherTool {
    type Bundle = InternalDev;
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

pub struct UppercaseTool;

#[async_trait]
impl BamlTool for UppercaseTool {
    type Bundle = InternalDev;
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

pub struct DelayedResponseTool;

#[async_trait]
impl BamlTool for DelayedResponseTool {
    type Bundle = InternalDev;
    const LOCAL_NAME: &'static str = "delayed_response";
    type OpenInput = ();
    type Input = DelayedInput;
    type Output = DelayedOutput;

    fn description(&self) -> &'static str {
        "Returns a response after a short delay (simulates async operation)"
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        use tokio::time::{Duration, sleep};
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

pub struct CalculatorTool;

#[async_trait]
impl BamlTool for CalculatorTool {
    type Bundle = InternalDev;
    const LOCAL_NAME: &'static str = "calculate";
    type OpenInput = ();
    type Input = CalculatorInput;
    type Output = CalculatorOutput;

    fn description(&self) -> &'static str {
        "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let left = args.expression.left as f64;
        let right = args.expression.right as f64;
        let (operation_symbol, result) = match args.expression.operation {
            MathOperation::Add => ("+", left + right),
            MathOperation::Subtract => ("-", left - right),
            MathOperation::Multiply => ("*", left * right),
            MathOperation::Divide => ("/", if right != 0.0 { left / right } else { 0.0 }),
        };
        let expr_str = format!("{} {} {}", left as i64, operation_symbol, right as i64);
        tracing::info!(expression = %expr_str, "CalculatorTool executed");
        Ok(CalculatorOutput {
            expression: expr_str.clone(),
            result,
            formatted: format!("{} = {}", expr_str, result),
        })
    }
}

#[derive(Clone)]
pub struct A2aInMemoryClient {
    target: Arc<dyn baml_rt::A2aRequestHandler>,
}

impl A2aInMemoryClient {
    pub fn new(target: Arc<dyn baml_rt::A2aRequestHandler>) -> Self {
        Self { target }
    }

    pub async fn send(&self, request: Value) -> Result<Vec<Value>> {
        let stream = self
            .target
            .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
            .await?;
        let chunks = baml_rt_core::collect_a2a_stream(stream).await;
        Ok(chunks
            .into_iter()
            .map(baml_rt_core::A2aStreamChunk::into_inner)
            .collect())
    }
}

pub struct A2aRelayTool {
    client: A2aInMemoryClient,
}

impl A2aRelayTool {
    pub fn new(client: A2aInMemoryClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl BamlTool for A2aRelayTool {
    type Bundle = InternalDev;
    const LOCAL_NAME: &'static str = "a2a_relay";
    type OpenInput = ();
    type Input = A2aRelayInput;
    type Output = A2aRelayOutput;

    fn description(&self) -> &'static str {
        "Relays an A2A request to another in-memory agent."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let handle = tokio::runtime::Handle::current();
        let responses = task::block_in_place(|| handle.block_on(self.client.send(args.request)))?;
        Ok(A2aRelayOutput { responses })
    }
}
