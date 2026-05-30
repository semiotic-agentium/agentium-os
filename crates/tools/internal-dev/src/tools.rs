// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Internal development tool implementations (testing and dev only).

use baml_derive::BamlType;
use baml_rt_tools::OpaqueJson;
use serde::{Deserialize, Serialize};

// ---------- Calculator types (moved from baml-rt-tools::support) ----------

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct Expression {
    pub left: i64,
    pub operation: MathOperation,
    pub right: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
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

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct CalculatorInput {
    pub expression: Expression,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct CalculatorOutput {
    pub expression: String,
    pub result: f64,
    pub formatted: String,
}

// ---------- Weather types ----------

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct WeatherInput {
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
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

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct UppercaseInput {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct UppercaseOutput {
    pub result: String,
    pub original: String,
}

// ---------- Delayed types ----------

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct DelayedInput {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct DelayedOutput {
    pub response: String,
    pub timestamp: String,
}

// ---------- A2A relay types ----------

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct A2aRelayInput {
    pub request: OpaqueJson,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct A2aRelayOutput {
    pub responses: Vec<OpaqueJson>,
}
