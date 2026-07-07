// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tier-3 gate authorization: suspend via A2A InputRequired, grant on resume.

use std::{collections::HashMap, sync::RwLock};

use baml_rt_core::context::RuntimeScope;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeAction {
    NoPending,
    Granted,
    Denied,
}

fn is_gate_denial(user_text: &str) -> bool {
    let t = user_text.trim().to_lowercase();
    if t.is_empty() {
        return false;
    }
    t.starts_with("[gate-deny]")
        || t == "deny"
        || t == "reject"
        || t.starts_with("deny ")
        || t.starts_with("reject ")
}

#[derive(Debug, Clone)]
struct PendingAuth {
    tool_name: String,
    args_fingerprint: String,
    granted: bool,
}

#[derive(Debug, Default)]
pub struct PendingGateAuthStore {
    inner: RwLock<HashMap<String, PendingAuth>>,
}

impl PendingGateAuthStore {
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

    fn fingerprint(tool_name: &str, args: &Value) -> String {
        format!(
            "{tool_name}:{}",
            serde_json::to_string(args).unwrap_or_default()
        )
    }

    pub fn set_pending(&self, scope: &RuntimeScope, tool_name: &str, args: &Value) {
        let key = Self::scope_key(scope);
        let entry = PendingAuth {
            tool_name: tool_name.to_string(),
            args_fingerprint: Self::fingerprint(tool_name, args),
            granted: false,
        };
        let mut guard = self.inner.write().expect("pending_auth lock");
        guard.insert(key, entry);
    }

    /// Called when the live stream resumes after `InputRequired` (user replied).
    pub fn resolve_on_resume(&self, scope: &RuntimeScope, user_text: &str) -> ResumeAction {
        let key = Self::scope_key(scope);
        let mut guard = self.inner.write().expect("pending_auth lock");
        let Some(entry) = guard.get_mut(&key) else {
            return ResumeAction::NoPending;
        };
        if is_gate_denial(user_text) {
            guard.remove(&key);
            return ResumeAction::Denied;
        }
        entry.granted = true;
        ResumeAction::Granted
    }

    #[deprecated(note = "use resolve_on_resume")]
    pub fn grant_on_resume(&self, scope: &RuntimeScope) {
        let _ = self.resolve_on_resume(scope, "approve");
    }

    pub fn is_granted(&self, scope: &RuntimeScope, tool_name: &str, args: &Value) -> bool {
        let key = Self::scope_key(scope);
        let mut guard = self.inner.write().expect("pending_auth lock");
        let Some(entry) = guard.get(&key) else {
            return false;
        };
        if entry.tool_name != tool_name
            || entry.args_fingerprint != Self::fingerprint(tool_name, args)
        {
            return false;
        }
        if !entry.granted {
            return false;
        }
        guard.remove(&key);
        true
    }
}
