use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::Result;
use baml_rt_tools::{
    baml_tool,
    bundles::Support,
    tools::BamlTool,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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

/// Calculator tool (support/calculate) for fixture and demo use.
#[derive(Default)]
pub struct CalculatorTool;

#[baml_tool(
    name = "support/calculate",
    description = "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division.",
    tags = ["support", "calculate"],
    baml_types = [Expression, MathOperation, CalculatorInput, CalculatorOutput],
)]
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
