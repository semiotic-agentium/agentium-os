//! Helpers for Notion / ClickUp / Slack integration test binaries only.
//!
//! This module is declared only from those `tests/runner_*` roots—not from `common/mod.rs`—so
//! `cargo clippy --all-features --tests` does not compile these items into crates that only use
//! `common` (avoiding `dead_code` when package features are unified across integration tests).

use serde_json::Value;

/// Walk nested JSON for a string scalar matching `key` == `expected` (ignores numbers/bools).
pub fn contains_kv(value: &Value, key: &str, expected: &str) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(k, v)| {
            (k == key && v.as_str() == Some(expected)) || contains_kv(v, key, expected)
        }),
        Value::Array(items) => items.iter().any(|v| contains_kv(v, key, expected)),
        _ => false,
    }
}

impl crate::common::RunningHttpServer {
    /// Mock APIs mount under a path prefix; `start_http_server` binds `/`.
    pub fn with_base_path(mut self, base_path: &str) -> Self {
        let trimmed = base_path.trim();
        if trimmed.is_empty() || trimmed == "/" {
            return self;
        }
        if trimmed.starts_with('/') {
            self.base_url.push_str(trimmed);
        } else {
            self.base_url.push('/');
            self.base_url.push_str(trimmed);
        }
        self
    }
}
