// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Eval lifecycle helpers and effect-gated timeout policy.
//!
//! Shared direct-code wrapper and code normalization so `evaluate()` and
//! `prepare_brief_poll_eval` use the same token/slot/notify pattern.

use std::sync::Arc;

use baml_rt_core::{bus::EffectLiveness, ids::ContextId};

// ---------- Shared eval wrapper (direct code + token) ----------

/// Normalize user code to an expression body: trim and wrap in IIFE if not already wrapped.
pub(crate) fn normalize_code_to_expr_body(code: &str) -> String {
    let code_trimmed = code.trim_start();
    let is_arrow_iife = code_trimmed.starts_with("(()") || code_trimmed.starts_with("(async ()");
    let already_wrapped = code_trimmed.starts_with("(function()")
        || code_trimmed.starts_with("(async function()")
        || is_arrow_iife;
    if already_wrapped {
        code_trimmed.to_string()
    } else {
        format!("(function() {{ {} }})()", code)
    }
}

/// Build the direct eval JS that runs `code_expr_body` and either returns a sync string
/// or installs __set_eval_result(token, ...) for promises and returns "__EVAL_PROMISE_PENDING__".
pub(crate) fn build_eval_direct_code(code_expr_body: &str, token_literal: &str) -> String {
    format!(
        r#"(function() {{
var __r = {code};
if (__r && typeof __r.then === 'function') {{
  Promise.resolve(__r).then(function(__v) {{
    var __json;
    if (typeof __v === 'string') {{
      __json = __v;
    }} else if (typeof __v === 'undefined') {{
      __json = "{{}}";
    }} else {{
      __json = JSON.stringify(__v);
      if (typeof __json === 'undefined') {{
        __json = "{{}}";
      }}
    }}
    __set_eval_result("{token}", __json);
  }}).catch(function(__e) {{
    __set_eval_result("{token}", JSON.stringify({{ error: (__e && __e.toString ? __e.toString() : String(__e)) }}));
  }});
  return "__EVAL_PROMISE_PENDING__";
}}
if (typeof __r === 'string') {{ return __r; }}
if (typeof __r === 'undefined') {{ return "{{}}"; }}
var __sync_json = JSON.stringify(__r);
if (typeof __sync_json === 'undefined') {{ return "{{}}"; }}
return __sync_json;
}})()"#,
        code = code_expr_body,
        token = token_literal,
    )
}

/// Encapsulates effect-gated timeout logic for promise polling.
///
/// Determines timeout attempts based on whether effects are in-flight:
/// - Effects active: use max_attempts (configurable, default 30 minutes) to allow I/O to complete
/// - No effects: use idle_timeout_attempts (default 5s) to detect deadlocks
pub struct EffectGatedTimeoutPolicy {
    liveness: Arc<dyn EffectLiveness>,
    context_id: ContextId,
    idle_timeout_attempts: u32,
    max_attempts: u32,
}

impl EffectGatedTimeoutPolicy {
    /// Default maximum attempts when effects are in-flight (30 minutes)
    pub const DEFAULT_MAX_ATTEMPTS: u32 = 1_800_000;

    pub fn new(
        liveness: Arc<dyn EffectLiveness>,
        context_id: ContextId,
        idle_timeout_ms: u64,
        max_attempts_ms: u64,
    ) -> Self {
        Self {
            liveness,
            context_id,
            idle_timeout_attempts: idle_timeout_ms as u32,
            max_attempts: max_attempts_ms as u32,
        }
    }

    /// Get the timeout attempts based on current effect state.
    ///
    /// Returns max_attempts if downstream progress-capable effects are in-flight,
    /// otherwise idle_timeout_attempts.
    pub async fn timeout_attempts(&self) -> u32 {
        let counts = self.liveness.in_flight(&self.context_id).await;
        if counts.has_progress_effects() {
            // Effects active: use long timeout
            self.max_attempts
        } else {
            // No effects: use short idle timeout
            self.idle_timeout_attempts
        }
    }
}
