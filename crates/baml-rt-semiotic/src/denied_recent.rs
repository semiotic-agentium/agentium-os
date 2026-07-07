// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Denied→executed diff for friction_denial / prevented_error telemetry.
//!
//! Port of `sc-review/plugin/scripts/sc_telemetry.py` (structure-only, in-memory).

use std::{
    collections::{BTreeSet, HashMap},
    sync::RwLock,
    time::{Duration, Instant},
};

use baml_rt_core::context::RuntimeScope;
use serde_json::Value;

const DENIED_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_DENIED: usize = 8;
const RELATED: f32 = 0.35;
const MINOR: f32 = 0.70;
const IDENTICAL: f32 = 0.95;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelemetryVerdict {
    FrictionDenial {
        secs_to_retry: u32,
    },
    PreventedError {
        diff: &'static str,
        secs_to_reground: u32,
    },
}

#[derive(Debug)]
struct DeniedCall {
    at: Instant,
    tool_class: String,
    tier: u8,
    tokens: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub struct DeniedRecentStore {
    inner: RwLock<HashMap<String, Vec<DeniedCall>>>,
}

impl DeniedRecentStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn scope_key(scope: &RuntimeScope) -> String {
        format!(
            "{}:{}",
            scope.agent_id().as_str(),
            scope.task_id_opt().map(|t| t.as_str()).unwrap_or("")
        )
    }

    pub fn record_denied(&self, scope: &RuntimeScope, tool_name: &str, args: &Value, tier: u8) {
        let key = Self::scope_key(scope);
        let now = Instant::now();
        let mut guard = self.inner.write().expect("denied_recent lock");
        let items = guard.entry(key).or_default();
        items.retain(|i| now.duration_since(i.at) < DENIED_TTL);
        if items.len() >= MAX_DENIED {
            items.remove(0);
        }
        items.push(DeniedCall {
            at: now,
            tool_class: tool_class(tool_name),
            tier,
            tokens: norm_tokens(&call_text(tool_name, args)),
        });
    }

    pub fn match_executed(
        &self,
        scope: &RuntimeScope,
        tool_name: &str,
        args: &Value,
        tier: u8,
    ) -> Option<TelemetryVerdict> {
        let key = Self::scope_key(scope);
        let now = Instant::now();
        let toks = norm_tokens(&call_text(tool_name, args));
        let tc = tool_class(tool_name);
        let mut guard = self.inner.write().expect("denied_recent lock");
        let items = guard.get_mut(&key)?;
        items.retain(|i| now.duration_since(i.at) < DENIED_TTL);
        let mut best_score = 0.0f32;
        let mut best_idx = None;
        for (idx, item) in items.iter().enumerate() {
            if item.tool_class != tc {
                continue;
            }
            let score = jaccard(&toks, &item.tokens);
            if score > best_score {
                best_score = score;
                best_idx = Some(idx);
            }
        }
        let idx = best_idx?;
        if best_score < RELATED {
            return None;
        }
        let denied = items.remove(idx);
        let elapsed = now.duration_since(denied.at).as_secs() as u32;
        let _denied_tier = denied.tier.max(tier);
        if best_score >= IDENTICAL {
            Some(TelemetryVerdict::FrictionDenial {
                secs_to_retry: elapsed,
            })
        } else {
            let diff = if best_score >= MINOR {
                "minor"
            } else {
                "major"
            };
            Some(TelemetryVerdict::PreventedError {
                diff,
                secs_to_reground: elapsed,
            })
        }
    }
}

/// Pure diff for aggregate replay (no wall-clock TTL).
pub fn diff_executed_against_denied(
    denied_tool: &str,
    denied_args: &Value,
    executed_tool: &str,
    executed_args: &Value,
) -> Option<TelemetryVerdict> {
    if tool_class(denied_tool) != tool_class(executed_tool) {
        return None;
    }
    let denied_toks = norm_tokens(&call_text(denied_tool, denied_args));
    let executed_toks = norm_tokens(&call_text(executed_tool, executed_args));
    let score = jaccard(&denied_toks, &executed_toks);
    if score < RELATED {
        return None;
    }
    if score >= IDENTICAL {
        Some(TelemetryVerdict::FrictionDenial { secs_to_retry: 0 })
    } else {
        let diff = if score >= MINOR { "minor" } else { "major" };
        Some(TelemetryVerdict::PreventedError {
            diff,
            secs_to_reground: 0,
        })
    }
}

pub fn tool_class(tool_name: &str) -> String {
    if tool_name == "bash" || tool_name == "Bash" {
        return "bash".into();
    }
    if let Some(rest) = tool_name.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or("mcp");
        return format!("mcp:{server}");
    }
    "other".into()
}

fn call_text(tool_name: &str, args: &Value) -> String {
    format!(
        "{tool_name} {}",
        serde_json::to_string(args).unwrap_or_default()
    )
}

fn norm_tokens(text: &str) -> BTreeSet<String> {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| {
            !c.is_ascii_alphanumeric() && c != '_' && c != '.' && c != '/' && c != ':' && c != '-'
        })
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    inter / union
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
    use serde_json::json;

    use super::*;

    fn scope() -> RuntimeScope {
        RuntimeScope::task_scope(
            ContextId::from("ctx-1"),
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap()),
            MessageId::from("msg-1"),
            TaskId::from_external(ExternalId::new("task-1".to_string())),
        )
    }

    #[test]
    fn identical_retry_is_friction_denial() {
        let store = DeniedRecentStore::new();
        let args = json!({"command": "rm -rf /tmp/x"});
        store.record_denied(&scope(), "bash", &args, 2);
        let verdict = store.match_executed(&scope(), "bash", &args, 2);
        assert!(matches!(
            verdict,
            Some(TelemetryVerdict::FrictionDenial { .. })
        ));
    }

    #[test]
    fn unrelated_executed_no_match() {
        let store = DeniedRecentStore::new();
        store.record_denied(
            &scope(),
            "bash",
            &json!({"command": "sqlite3 prod.db 'DELETE FROM users WHERE id=1'"}),
            2,
        );
        assert!(
            store
                .match_executed(&scope(), "git", &json!({"command": "status"}), 2)
                .is_none()
        );
    }
}
