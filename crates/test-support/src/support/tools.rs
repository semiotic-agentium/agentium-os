//! Test tool implementations for testing BAML tool system.

use async_trait::async_trait;
use baml_rt::{Result, tools::BamlTool};
use baml_rt_tools::bundles::Support;
use baml_tools_calculator::{CalculatorInput, CalculatorOutput, MathOperation};

/// Example calculator tool
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
