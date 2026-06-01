// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Canonical wire schema identifiers and shared envelope headers for host event delivery.

use serde::{Deserialize, Serialize};

use crate::event_subscription::{EventSchemaVersion, EventSourceKey, EventSourceKind};

/// Well-known wire schema version strings for host→agent dispatch.
pub mod wire {
    /// Raw source-record batches (Slack, ClickUp, GitHub Issues, …).
    pub const HOST_SOURCE_RECORDS_V1: &str = "host.source-records.v1";
    /// Durable system callback dispatch payloads.
    pub const SYSTEM_CALLBACK_V1: &str = "system.callback.v1";
    /// Default routing key for [`HOST_SOURCE_RECORDS_V1`] intake delivery.
    pub const SOURCE_RECORDS_ROUTING_KEY: &str = "event:intake";
}

/// Parsed `host.source-records.v1` schema version.
pub fn host_source_records_schema_version() -> EventSchemaVersion {
    EventSchemaVersion::parse(wire::HOST_SOURCE_RECORDS_V1)
        .expect("HOST_SOURCE_RECORDS_V1 is a valid EventSchemaVersion")
}

/// Source identity shared by all `host.source-records.v1` batch payloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostSourceDescriptor {
    pub source_kind: EventSourceKind,
    pub source_key: EventSourceKey,
    pub source_label: String,
}

/// Header fields common to every `host.source-records.v1` message body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostSourceRecordsEnvelopeHeader {
    pub schema_version: EventSchemaVersion,
    pub emitted_at_unix: u64,
    pub source: HostSourceDescriptor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_constants_parse() {
        assert!(EventSchemaVersion::parse(wire::HOST_SOURCE_RECORDS_V1).is_some());
        assert!(EventSchemaVersion::parse(wire::SYSTEM_CALLBACK_V1).is_some());
        assert!(EventSourceKind::parse("slack").is_some());
    }
}
