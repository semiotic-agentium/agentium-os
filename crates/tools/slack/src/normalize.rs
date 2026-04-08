use baml_rt_core::event_subscription::EventSourceKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SlackTransportKind {
    Polling,
    EventsApiHttp,
    SocketMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SlackTransportAuthorization {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enterprise_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_bot: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_enterprise_install: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlackTransportMetadata {
    pub kind: SlackTransportKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_at_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_num: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorizations: Vec<SlackTransportAuthorization>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlackNormalizedSource {
    pub source_kind: String,
    pub source_key: EventSourceKey,
    pub source_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlackNormalizedMessageRecord {
    pub channel_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_reply: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_users: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "record_kind")]
pub enum SlackNormalizedRecord {
    #[serde(rename = "slack.message")]
    Message(SlackNormalizedMessageRecord),
}

impl SlackNormalizedRecord {
    pub fn message(record: SlackNormalizedMessageRecord) -> Self {
        Self::Message(record)
    }

    pub fn as_message(&self) -> Option<&SlackNormalizedMessageRecord> {
        match self {
            Self::Message(record) => Some(record),
        }
    }

    pub fn source_ref(&self) -> Option<&str> {
        self.as_message()
            .and_then(|record| record.source_ref.as_deref())
    }

    pub fn text(&self) -> Option<&str> {
        self.as_message().and_then(|record| record.text.as_deref())
    }

    pub fn user(&self) -> Option<&str> {
        self.as_message().and_then(|record| record.user.as_deref())
    }

    pub fn raw(&self) -> &Value {
        match self {
            Self::Message(record) => &record.raw,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlackNormalizedBatch {
    pub schema_version: String,
    pub emitted_at_unix: u64,
    pub source: SlackNormalizedSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<SlackTransportMetadata>,
    pub records: Vec<SlackNormalizedRecord>,
}

pub fn normalize_polling_batch(
    schema_version: &str,
    channel_id: &str,
    source_key: &EventSourceKey,
    source_label: &str,
    messages: &[Value],
    emitted_at_unix: u64,
) -> SlackNormalizedBatch {
    SlackNormalizedBatch {
        schema_version: schema_version.to_string(),
        emitted_at_unix,
        source: SlackNormalizedSource {
            source_kind: "slack".to_string(),
            source_key: source_key.clone(),
            source_label: source_label.to_string(),
        },
        transport: Some(SlackTransportMetadata {
            kind: SlackTransportKind::Polling,
            delivery_id: None,
            received_at_unix: Some(emitted_at_unix),
            team_id: None,
            api_app_id: None,
            event_context: None,
            retry_num: None,
            retry_reason: None,
            authorizations: Vec::new(),
        }),
        records: messages
            .iter()
            .map(|message| normalize_polling_record(channel_id, message))
            .collect(),
    }
}

fn normalize_polling_record(channel_id: &str, message: &Value) -> SlackNormalizedRecord {
    let user_id = string_field(message, "user");
    let ts = string_field(message, "ts");

    SlackNormalizedRecord::message(SlackNormalizedMessageRecord {
        channel_id: channel_id.to_string(),
        ts: ts.clone(),
        thread_ts: string_field(message, "thread_ts"),
        reply_count: message.get("reply_count").and_then(Value::as_u64),
        latest_reply: string_field(message, "latest_reply"),
        reply_users: string_array_field(message, "reply_users"),
        user_id,
        user: None,
        user_name: string_field(message, "user_name")
            .or_else(|| nested_string_field(message, &["user_profile", "display_name"])),
        username: string_field(message, "username"),
        bot_id: string_field(message, "bot_id"),
        text: string_field(message, "text"),
        subtype: string_field(message, "subtype"),
        source_ref: ts
            .as_deref()
            .map(|value| slack_source_ref(channel_id, value)),
        permalink: string_field(message, "permalink"),
        // Preserve the original Slack message payload at the raw boundary so
        // semantic ingress and future source-family logic can recover fields
        // that this normalized projection does not surface explicitly.
        raw: message.clone(),
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn nested_string_field(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(str::to_string)
}

fn string_array_field(value: &Value, key: &str) -> Option<Vec<String>> {
    let values = value.get(key).and_then(Value::as_array).map(|items| {
        let mut had_non_string = false;
        let parsed = items
            .iter()
            .filter_map(|item| match item.as_str() {
                Some(value) => Some(value.to_string()),
                None => {
                    had_non_string = true;
                    None
                }
            })
            .collect::<Vec<_>>();
        if had_non_string {
            warn!(
                field = %key,
                "support/slack normalization dropped non-string values from array field"
            );
        }
        parsed
    })?;
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn slack_source_ref(channel_id: &str, ts: &str) -> String {
    let mut compact_ts = String::with_capacity(ts.len());
    for ch in ts.chars() {
        if ch != '.' {
            compact_ts.push(ch);
        }
    }
    format!("slack://channel/{channel_id}/p{compact_ts}")
}
