//! Tool-facing types for the Claude bundle.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use ts_rs::TS;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[ts(export)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaudeCompletion {
    #[default]
    Done,
    InputRequired,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudeUserContentBlockDto {
    Text { text: String },
    ImageUrl { url: String },
    ImageBase64 { media_type: String, data: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolSendInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ClaudeUserContentBlockDto>>,
    /// Structured input (e.g. UserInput::ToolApproval). When present, session uses the right type; for ToolApproval, prompt or display_text is sent and permission is applied. Not in TS/schema (opaque JSON).
    #[serde(skip_serializing_if = "Option::is_none", rename = "userInput")]
    #[ts(skip)]
    #[schemars(skip)]
    pub user_input: Option<JsonValue>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolNextOutput {
    pub events: Vec<ClaudeEventDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<ClaudeCompletion>,
}
