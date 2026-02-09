use crate::register_tool_metadata;
use crate::tools::ToolFunctionMetadata;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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

pub fn support_calculate_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    // This is a compile-time constant, so parsing cannot fail
    let (name, class_name) = parse_tool_name_and_class("support/calculate")
        .expect("support/calculate is a compile-time constant and must be valid");
    TypeBasedMetadataBuilder::<(), CalculatorInput, CalculatorOutput>::new(
        name.clone(),
        class_name,
        "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division.".to_string(),
    )
    .with_tags(vec!["support".to_string(), "calculate".to_string()])
    .build_metadata()
}

register_tool_metadata!(support_calculate_metadata);
