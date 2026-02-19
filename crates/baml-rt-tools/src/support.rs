use crate::bundles::Support;
use crate::register_tool;
use crate::tools::{BamlTool, ToolFunctionMetadata, ToolHandler, create_tool_handler};
use async_trait::async_trait;
use baml_derive::BamlType;
use baml_derive_core::BamlType as BamlTypeTrait;
use baml_rt_core::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct Expression {
    pub left: i64,
    pub operation: MathOperation,
    pub right: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub enum MathOperation {
    #[serde(alias = "+")]
    #[baml(alias = "+")]
    Add,
    #[serde(alias = "-")]
    #[baml(alias = "-")]
    Subtract,
    #[serde(alias = "*")]
    #[baml(alias = "*")]
    Multiply,
    #[serde(alias = "/")]
    #[baml(alias = "/")]
    Divide,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct CalculatorInput {
    pub expression: Expression,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
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

    let baml_decl = [
        Expression::baml_decl(),
        MathOperation::baml_decl(),
        CalculatorInput::baml_decl(),
        CalculatorOutput::baml_decl(),
    ]
    .join("\n\n");

    TypeBasedMetadataBuilder::<(), CalculatorInput, CalculatorOutput>::new(
        name.clone(),
        class_name,
        "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division.".to_string(),
    )
    .with_baml_decl(baml_decl)
    .with_tags(vec!["support".to_string(), "calculate".to_string()])
    .build_metadata()
}

fn support_calculate_build() -> Result<Arc<dyn ToolHandler>> {
    create_tool_handler(CalculatorTool).map(|(_, h)| h)
}

register_tool!(support_calculate_metadata, support_calculate_build);

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
