// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Templates for the external-tool scaffold (v2).
//!
//! TypeScript starters use the official MCP SDK. Karakara packages the MCP
//! server as a toolset after reading `tools/list`. The retired v1 scaffold
//! emitted hand-written schemas, a server wrapper, and rootfs helpers.

pub mod lang;

/// File emitted by the scaffold.
#[derive(Debug, Clone)]
pub struct GeneratedFile {
    pub relative_path: String,
    pub content: String,
}

impl GeneratedFile {
    pub fn new(relative_path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            content: content.into(),
        }
    }
}

/// Default bundle namespace when the user doesn't supply `--bundle`.
///
/// `support/` is the convention for business integrations (ClickUp, Notion,
/// Slack). Any non-empty, slash-free string is valid via
/// [`baml_rt_tools::BundleName`].
pub const DEFAULT_BUNDLE: &str = "support";

/// Fully-qualified tool id (`bundle/name`).
pub fn tool_id(bundle: &str, name: &str) -> String {
    format!("{bundle}/{name}")
}
