//! Internal development tool implementations (testing and dev only).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

// ---------- Calculator types (moved from baml-rt-tools::support) ----------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct Expression {
    pub left: i64,
    pub operation: MathOperation,
    pub right: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub enum MathOperation {
    #[serde(alias = "+")]
    Add,
    #[serde(alias = "-")]
    Subtract,
    #[serde(alias = "*")]
    Multiply,
    #[serde(alias = "/")]
    Divide,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct CalculatorInput {
    pub expression: Expression,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct CalculatorOutput {
    pub expression: String,
    pub result: f64,
    pub formatted: String,
}

// ---------- Weather types ----------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct WeatherInput {
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct WeatherOutput {
    pub location: String,
    pub temperature: String,
    pub temperature_f: i64,
    pub condition: String,
    pub humidity: String,
    pub wind_speed: String,
    pub description: String,
}

// ---------- Uppercase types ----------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct UppercaseInput {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct UppercaseOutput {
    pub result: String,
    pub original: String,
}

// ---------- Delayed types ----------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct DelayedInput {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct DelayedOutput {
    pub response: String,
    pub timestamp: String,
}

// ---------- A2A relay types ----------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct A2aRelayInput {
    #[ts(type = "any")]
    pub request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct A2aRelayOutput {
    #[ts(type = "any[]")]
    pub responses: Vec<Value>,
}
