//! Shared event schema metadata surfaced by the SDK CLI.

/// A schema version the CLI can describe to users when scaffolding subscriptions.
#[derive(Debug, Clone, Copy)]
pub struct KnownEventSchema {
    pub version: &'static str,
    pub description: &'static str,
}

pub const KNOWN_EVENT_SCHEMAS: &[KnownEventSchema] = &[
    KnownEventSchema {
        version: "host.source-records.v1",
        description: "Generic raw source-ingress batch produced by host-managed event sources",
    },
    KnownEventSchema {
        version: "system.callback.v1",
        description: "Durable host-native callback event emitted by system/callback",
    },
    KnownEventSchema {
        version: "task-daemon.interpretation.v1",
        description: "Task daemon interpretation event for compatibility and migration",
    },
];
