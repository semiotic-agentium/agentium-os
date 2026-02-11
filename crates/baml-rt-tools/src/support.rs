use async_trait::async_trait;
use baml_rt_core::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::bundles::Support;
use crate::register_tool_metadata;
use crate::tools::{BamlTool, ToolFunctionMetadata};

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

/// Calculator tool (support/calculate) for fixture and demo use.
pub struct CalculatorTool;

#[async_trait]
impl BamlTool for CalculatorTool {
    type Bundle = Support;
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
            MathOperation::Divide => {
                if right == 0.0 {
                    return Err(baml_rt_core::BamlRtError::InvalidArgument(
                        "division by zero".into(),
                    ));
                }
                ("/", left / right)
            }
        };
        let expr_str = format!(
            "{} {} {}",
            args.expression.left, operation_symbol, args.expression.right
        );
        Ok(CalculatorOutput {
            expression: expr_str.clone(),
            result,
            formatted: format!("{} = {}", expr_str, result),
        })
    }
}
