// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Internal development tool bundle for BAML runtime tests.
//!
//! This crate provides the `internal-dev` bundle, tool types, and metadata registration.
//! BamlTool implementations live in test-support (to avoid baml-rt dependency cycle).

pub mod bundle;
mod metadata;
pub mod tools;

pub use bundle::InternalDev;
pub use tools::{
    A2aRelayInput, A2aRelayOutput, CalculatorInput, CalculatorOutput, DelayedInput, DelayedOutput,
    Expression, MathOperation, UppercaseInput, UppercaseOutput, WeatherInput, WeatherOutput,
};
