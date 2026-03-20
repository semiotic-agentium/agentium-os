use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::Result;
use baml_rt_tools::{baml_tool, bundles::Support, tools::BamlTool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A binary arithmetic expression: left OP right.
/// Provide integer operands and choose one of the four arithmetic operations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct Expression {
    #[baml(description = "Left-hand integer operand.")]
    pub left: i64,
    #[baml(description = "Arithmetic operation to apply.")]
    pub operation: MathOperation,
    #[baml(description = "Right-hand integer operand. Must not be 0 when operation is Divide.")]
    pub right: i64,
}

/// Arithmetic operation for a binary expression.
/// Accepted as the variant name (e.g. 'Add') or its symbolic alias ('+', '-', '*', '/').
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

/// Input to the calculator tool. Construct an Expression and submit it to evaluate.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct CalculatorInput {
    #[baml(description = "The binary expression to evaluate.")]
    pub expression: Expression,
}

impl baml_rt_tools::DescribeAction for CalculatorInput {
    fn describe(&self) -> String {
        let e = &self.expression;
        let op = match e.operation {
            MathOperation::Add => "+",
            MathOperation::Subtract => "-",
            MathOperation::Multiply => "*",
            MathOperation::Divide => "/",
        };
        format!("computing {} {} {}", e.left, op, e.right)
    }
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

    fn describe_open(&self) -> String {
        "using calculator for arithmetic".to_string()
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
