//! Event source type descriptor for system callbacks (`system.callback.v1`).

use std::sync::OnceLock;

use baml_rt_core::{AgentDispatchRoutingKey, EventSchemaVersion, EventSourceKind, host_wire::wire};
use baml_rt_tools::{EventSourceTypeDescriptor, EventSourceTypeDescriptorProvider};
use serde_json::Value;

use crate::{
    callback_dispatch_message::{
        SystemCallbackDispatchMessage, system_callback_dispatch_json_schema,
    },
    callback_producer::{CALLBACK_EVENT_ROUTING_KEY, CALLBACK_SOURCE_KIND},
};

fn callback_sample_payload() -> Value {
    serde_json::to_value(SystemCallbackDispatchMessage::token_probe_sample(
        "dispatch-echo:callback:probe",
        "probe",
    ))
    .expect("serialize callback sample")
}

fn system_callback_descriptor() -> EventSourceTypeDescriptor {
    EventSourceTypeDescriptor {
        descriptor_id: "system-callback-token",
        tool_name: "system/callback",
        source_kind: EventSourceKind::parse(CALLBACK_SOURCE_KIND).expect("system/callback"),
        wire_schema: EventSchemaVersion::parse(wire::SYSTEM_CALLBACK_V1).expect("wire schema"),
        default_routing_key: AgentDispatchRoutingKey::parse(CALLBACK_EVENT_ROUTING_KEY)
            .expect("routing key"),
        display_name: "System callback token",
        description: "Durable host callback dispatch with opaque token payload.",
        payload_name: "Callback payload",
        json_schema: system_callback_dispatch_json_schema,
        sample_payload: callback_sample_payload,
    }
}

fn system_callback_descriptors() -> &'static [EventSourceTypeDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<EventSourceTypeDescriptor>> = OnceLock::new();
    DESCRIPTORS
        .get_or_init(|| vec![system_callback_descriptor()])
        .as_slice()
}

inventory::submit! {
    EventSourceTypeDescriptorProvider {
        tool_name: "system/callback",
        descriptors: || system_callback_descriptors(),
    }
}
