// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry span helpers for the Slack BAML tool.

use tracing::Span;

/// Root span for [`super::SlackTool::execute`] dispatch.
#[inline]
pub(crate) fn execute(action: &str) -> Span {
    tracing::debug_span!("baml_tools_slack.execute", action = action)
}
