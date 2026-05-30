// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Runtime scope attribute helpers for OpenTelemetry spans and provenance.
//!
//! This module provides shared utilities for extracting runtime scope context
//! (context_id, message_id, task_id) for both OTEL spans and provenance events,
//! ensuring semantic alignment between tracing and provenance.

use baml_rt_core::context::RuntimeScope;

/// Extract runtime scope attributes for OpenTelemetry spans.
///
/// Returns a tuple of (context_id, message_id, task_id) as strings suitable
/// for span attributes. Uses structured fields following OTEL conventions.
#[inline]
pub fn scope_attributes(
    scope: Option<&RuntimeScope>,
) -> (Option<String>, Option<String>, Option<String>) {
    match scope {
        Some(scope) => (
            Some(scope.context_id().as_str().to_string()),
            Some(scope.message_id().as_str().to_string()),
            scope.task_id_opt().map(|id| id.as_str().to_string()),
        ),
        None => (None, None, None),
    }
}

/// Format scope attributes for structured logging.
///
/// Returns a formatted string suitable for log messages, showing
/// which scope identifiers are present.
#[inline]
pub fn scope_summary(scope: Option<&RuntimeScope>) -> String {
    let (ctx_id, msg_id, task_id) = scope_attributes(scope);
    let mut parts = Vec::new();
    if let Some(id) = ctx_id {
        parts.push(format!("context_id={}", id));
    }
    if let Some(id) = msg_id {
        parts.push(format!("message_id={}", id));
    }
    if let Some(id) = task_id {
        parts.push(format!("task_id={}", id));
    }
    if parts.is_empty() {
        "no_scope".to_string()
    } else {
        parts.join(", ")
    }
}
