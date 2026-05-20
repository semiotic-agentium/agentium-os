// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared event schema metadata surfaced by the SDK CLI.

/// A schema version the CLI can describe to users when scaffolding subscriptions.
#[derive(Debug, Clone, Copy)]
pub struct KnownEventSchema {
    pub version: &'static str,
    pub description: &'static str,
}

/// A compatibility source kind surfaced by the CLI even when it is not declared by a tool.
#[derive(Debug, Clone, Copy)]
pub struct KnownCompatibilitySourceKind {
    pub kind: &'static str,
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
];

pub const KNOWN_COMPATIBILITY_SOURCE_KINDS: &[KnownCompatibilitySourceKind] = &[
    KnownCompatibilitySourceKind {
        kind: "slack",
        description: "Slack source records polled by task-daemon",
    },
    KnownCompatibilitySourceKind {
        kind: "clickup",
        description: "ClickUp lifecycle source records polled by task-daemon",
    },
    KnownCompatibilitySourceKind {
        kind: "github_issues",
        description: "GitHub issue source records polled by task-daemon",
    },
];
