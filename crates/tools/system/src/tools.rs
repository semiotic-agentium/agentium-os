//! Tool-facing types for the system bundle (id-free, structured).
//!
//! These types are used for schema/TS generation and at the tool boundary.
//! No JSON-RPC or runtime IDs are exposed.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct InternalA2aTarget {
    pub agent_package: String,
    pub agent_instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct InternalA2aOpenInput {
    pub target: InternalA2aTarget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct InternalA2aSendInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<ConversationPart>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<ConversationPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ConversationChunk {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<ConversationMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_update: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_update: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct InternalA2aNextOutput {
    pub chunks: Vec<ConversationChunk>,
}
