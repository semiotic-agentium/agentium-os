use crate::tools::ToolFunctionMetadata;
use crate::{json_schema_value, ts_decl, ts_name, ToolName, ToolTypeSpec};
use crate::register_tool_metadata;
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
    let name = ToolName::parse("support/calculate")
        .expect("support/calculate must be a valid tool name");
    let class_name = ToolFunctionMetadata::derive_class_name(name.bundle(), name.local());
    ToolFunctionMetadata {
        name: name.clone(),
        class_name,
        description: "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division.".to_string(),
        open_input_schema: json_schema_value::<()>(),
        input_schema: json_schema_value::<CalculatorInput>(),
        output_schema: json_schema_value::<CalculatorOutput>(),
        open_input_type: ToolTypeSpec {
            name: ts_name::<()>(),
            ts_decl: ts_decl::<()>(),
        },
        input_type: ToolTypeSpec {
            name: ts_name::<CalculatorInput>(),
            ts_decl: ts_decl::<CalculatorInput>(),
        },
        output_type: ToolTypeSpec {
            name: ts_name::<CalculatorOutput>(),
            ts_decl: ts_decl::<CalculatorOutput>(),
        },
        tags: vec!["support".to_string(), "calculate".to_string()],
        secret_requirements: Vec::new(),
        // ALL Rust tools are host tools - they must be declared in manifest.json
        origin: crate::tools::ToolOrigin::Host,
    }
}

register_tool_metadata!(support_calculate_metadata);
