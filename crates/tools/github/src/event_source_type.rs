// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Event source type descriptor for GitHub Issues (`host.source-records.v1`).

use std::sync::OnceLock;

use baml_rt_core::{AgentDispatchRoutingKey, EventSchemaVersion, EventSourceKind, host_wire::wire};
use baml_rt_tools::{EventSourceTypeDescriptor, EventSourceTypeDescriptorProvider};

use crate::source_records::{
    github_issues_source_records_json_schema, github_issues_source_records_sample_payload,
};

fn github_issues_descriptor() -> baml_rt_tools::EventSourceTypeDescriptor {
    baml_rt_tools::EventSourceTypeDescriptor {
        descriptor_id: "github-issues-source-records",
        tool_name: "support/github",
        source_kind: EventSourceKind::parse("github_issues").expect("github_issues"),
        wire_schema: EventSchemaVersion::parse(wire::HOST_SOURCE_RECORDS_V1).expect("wire schema"),
        default_routing_key: AgentDispatchRoutingKey::parse(wire::SOURCE_RECORDS_ROUTING_KEY)
            .expect("routing key"),
        display_name: "GitHub Issues source records",
        description: "GitHub Issues poll batch as host.source-records.v1.",
        payload_name: "Source records",
        json_schema: github_issues_source_records_json_schema,
        sample_payload: github_issues_source_records_sample_payload,
    }
}

fn github_issues_descriptors() -> &'static [EventSourceTypeDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<EventSourceTypeDescriptor>> = OnceLock::new();
    DESCRIPTORS
        .get_or_init(|| vec![github_issues_descriptor()])
        .as_slice()
}

inventory::submit! {
    EventSourceTypeDescriptorProvider {
        tool_name: "support/github",
        descriptors: || github_issues_descriptors(),
    }
}
