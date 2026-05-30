// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Manifest load, A2A delegation extract, and tool-session trace helpers (I/O-adjacent, not open-input schema).

use baml_rt_core::BamlRtError;
use serde_json::Value;

pub(crate) fn tool_session_trace_enabled() -> bool {
    std::env::var("BAML_TRACE_TOOL_SESSION").is_ok()
}

pub(crate) fn tool_session_trace(message: &str) {
    if tool_session_trace_enabled() {
        tracing::trace!(message = %message, "[tool-session-trace]");
    }
}

pub(crate) fn completion_error_from(err: &BamlRtError) -> BamlRtError {
    match err {
        BamlRtError::SessionLifecycle(lifecycle) => {
            BamlRtError::SessionLifecycle(lifecycle.clone())
        }
        BamlRtError::StepPlanCorrectable(r) => BamlRtError::StepPlanCorrectable(r.clone()),
        _ => BamlRtError::InvalidArgument(err.to_string()),
    }
}

/// Load a builder-generated JSON manifest from the project build directory.
pub(crate) fn load_build_manifest<T: serde::de::DeserializeOwned>(
    project_root: &std::path::Path,
    filename: &str,
) -> Option<T> {
    let path = project_root.join(filename);
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<T>(&s) {
            Ok(val) => Some(val),
            Err(e) => {
                tracing::error!(
                    path = %path.display(),
                    error = %e,
                    "{filename} has invalid format — rebuild the agent"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Could not read {filename}");
            None
        }
    }
}

/// `target.agent_package` from open_input for A2A delegation tools.
pub(crate) fn extract_delegation_target_from_open_input(
    tool_name: &str,
    open_input: &Value,
) -> Option<String> {
    const A2A_TOOLS: [&str; 3] = ["system/internal_a2a", "system/a2a", "support/a2aRelay"];
    if !A2A_TOOLS.contains(&tool_name) {
        return None;
    }
    let target = open_input
        .get("target")
        .and_then(|t| t.get("agent_package"))
        .and_then(Value::as_str)?;
    Some(target.to_string())
}
