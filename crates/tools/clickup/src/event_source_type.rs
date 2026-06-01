// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Event source type descriptor for ClickUp (`host.source-records.v1`).

use std::sync::OnceLock;

use baml_rt_core::{AgentDispatchRoutingKey, EventSchemaVersion, EventSourceKind, host_wire::wire};
use baml_rt_tools::{EventSourceTypeDescriptor, EventSourceTypeDescriptorProvider};

use crate::source_records::{
    clickup_source_records_json_schema, clickup_source_records_sample_payload,
};

fn clickup_descriptor() -> EventSourceTypeDescriptor {
    EventSourceTypeDescriptor {
        descriptor_id: "clickup-source-records",
        tool_name: "support/clickup",
        source_kind: EventSourceKind::parse("clickup").expect("clickup"),
        wire_schema: EventSchemaVersion::parse(wire::HOST_SOURCE_RECORDS_V1).expect("wire schema"),
        default_routing_key: AgentDispatchRoutingKey::parse(wire::SOURCE_RECORDS_ROUTING_KEY)
            .expect("routing key"),
        display_name: "ClickUp source records",
        description: "ClickUp lifecycle task batch as host.source-records.v1.",
        payload_name: "Source records",
        json_schema: clickup_source_records_json_schema,
        sample_payload: clickup_source_records_sample_payload,
    }
}

fn clickup_descriptors() -> &'static [EventSourceTypeDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<EventSourceTypeDescriptor>> = OnceLock::new();
    DESCRIPTORS
        .get_or_init(|| vec![clickup_descriptor()])
        .as_slice()
}

inventory::submit! {
    EventSourceTypeDescriptorProvider {
        tool_name: "support/clickup",
        descriptors: || clickup_descriptors(),
    }
}
