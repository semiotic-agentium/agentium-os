//! Event source type descriptor for Slack (`host.source-records.v1`).

use std::sync::OnceLock;

use baml_rt_core::{
    AgentDispatchRoutingKey, EventSchemaVersion, EventSourceKind,
    event_subscription::EventSourceKey, host_wire::wire,
};
use baml_rt_tools::{EventSourceTypeDescriptor, EventSourceTypeDescriptorProvider};
use serde_json::json;

use crate::{
    message_shapes::slack_normalized_batch_json_schema,
    normalize::{
        SlackNormalizedBatch, SlackNormalizedMessageRecord, SlackNormalizedRecord,
        SlackNormalizedSource,
    },
};

fn slack_sample_payload() -> serde_json::Value {
    let source_key = EventSourceKey::parse("slack:C012TEST001").expect("sample source key");
    let batch = SlackNormalizedBatch {
        schema_version: wire::HOST_SOURCE_RECORDS_V1.to_string(),
        emitted_at_unix: 1_735_720_111,
        source: SlackNormalizedSource {
            source_kind: "slack".to_string(),
            source_key: source_key.clone(),
            source_label: "#agentium-eng".to_string(),
        },
        transport: None,
        records: vec![SlackNormalizedRecord::message(
            SlackNormalizedMessageRecord {
                channel_id: "C012TEST001".to_string(),
                ts: Some("1735720111.000001".to_string()),
                thread_ts: None,
                reply_count: None,
                latest_reply: None,
                reply_users: None,
                user_id: Some("U123".to_string()),
                user: None,
                user_name: Some("Ada".to_string()),
                username: None,
                bot_id: None,
                text: Some("Please turn this Slack thread into a tracked task.".to_string()),
                subtype: None,
                source_ref: None,
                permalink: None,
                raw: json!({
                    "ts": "1735720111.000001",
                    "user": "U123",
                    "text": "Please turn this Slack thread into a tracked task."
                }),
            },
        )],
    };
    serde_json::to_value(&batch).expect("serialize slack sample batch")
}

fn slack_descriptor() -> EventSourceTypeDescriptor {
    EventSourceTypeDescriptor {
        descriptor_id: "slack-source-records",
        tool_name: "support/slack",
        source_kind: EventSourceKind::parse("slack").expect("slack"),
        wire_schema: EventSchemaVersion::parse(wire::HOST_SOURCE_RECORDS_V1).expect("wire schema"),
        default_routing_key: AgentDispatchRoutingKey::parse(wire::SOURCE_RECORDS_ROUTING_KEY)
            .expect("routing key"),
        display_name: "Slack raw source records",
        description: "Normalized Slack channel polling batch delivered as host.source-records.v1.",
        payload_name: "Source records",
        json_schema: slack_normalized_batch_json_schema,
        sample_payload: slack_sample_payload,
    }
}

fn slack_descriptors() -> &'static [EventSourceTypeDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<EventSourceTypeDescriptor>> = OnceLock::new();
    DESCRIPTORS
        .get_or_init(|| vec![slack_descriptor()])
        .as_slice()
}

inventory::submit! {
    EventSourceTypeDescriptorProvider {
        tool_name: "support/slack",
        descriptors: || slack_descriptors(),
    }
}
