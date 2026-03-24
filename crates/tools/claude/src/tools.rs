//! Tool-facing types for the Claude bundle.

use baml_derive::BamlType;
use baml_derive_core::{JsonSchemaType, TsType};
use baml_rt_tools::tools::HistoryContextV1;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaudeCompletion {
    #[default]
    Done,
    InputRequired,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudeEventDto {
    AssistantText {
        text: String,
    },
    AssistantThinking {
        thinking: String,
    },
    AssistantToolUse {
        id: String,
        name: String,
        input: String,
    },
    AssistantToolResult {
        tool_use_id: String,
        content: Option<String>,
        is_error: Option<bool>,
    },
    SystemNotice {
        subtype: String,
        cwd: String,
        model: String,
        data: Option<String>,
    },
    StreamEventRaw {
        event: String,
    },
    TerminalResult {
        subtype: String,
        is_error: bool,
        num_turns: u32,
        total_cost_usd: Option<f64>,
        result: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudeUserContentBlockDto {
    Text { text: String },
    ImageUrl { url: String },
    ImageBase64 { media_type: String, data: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolSendInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ClaudeUserContentBlockDto>>,
    /// Structured input (e.g. UserInput::ToolApproval). When present, session uses the right type; for ToolApproval, prompt or display_text is sent and permission is applied. Not in TS/schema (opaque JSON).
    #[serde(skip_serializing_if = "Option::is_none", rename = "userInput")]
    #[baml(skip)]
    pub user_input: Option<JsonValue>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolNextOutput {
    pub events: Vec<ClaudeEventDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<ClaudeCompletion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

// Manual JsonSchemaType + TsType for complex struct-variant enums.
// These are opaque streaming event types; schema is intentionally permissive (any).
impl JsonSchemaType for ClaudeEventDto {
    fn json_schema_inline() -> serde_json::Value {
        serde_json::json!({})
    }
}

impl TsType for ClaudeEventDto {
    fn ts_type_name() -> &'static str {
        "ClaudeEventDto"
    }
    fn ts_decl() -> Option<String> {
        None
    }
}

impl JsonSchemaType for ClaudeUserContentBlockDto {
    fn json_schema_inline() -> serde_json::Value {
        serde_json::json!({})
    }
}

impl TsType for ClaudeUserContentBlockDto {
    fn ts_type_name() -> &'static str {
        "ClaudeUserContentBlockDto"
    }
    fn ts_decl() -> Option<String> {
        None
    }
}
