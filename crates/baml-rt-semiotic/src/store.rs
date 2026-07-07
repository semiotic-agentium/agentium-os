// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::RwLock,
    time::{Duration, Instant},
};

use baml_rt_core::context::RuntimeScope;

use crate::schema::{ParseArtifact, Postcondition};

#[derive(Debug, Clone)]
pub struct GroundingRecord {
    pub artifact: ParseArtifact,
    pub submitted_at: Instant,
    pub consumed: bool,
    pub plan_step_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingPostconditions {
    pub postconditions: Vec<Postcondition>,
    pub tier: u8,
    pub cwd: Option<String>,
}

/// Host-held grounding artifacts scoped to agent + task.
#[derive(Debug, Default)]
pub struct GroundingStore {
    inner: RwLock<HashMap<String, GroundingRecord>>,
    pending: RwLock<HashMap<String, PendingPostconditions>>,
}

impl GroundingStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(scope: &RuntimeScope) -> String {
        format!(
            "{}:{}",
            scope.agent_id().as_str(),
            scope.task_id_opt().map(|t| t.as_str()).unwrap_or("")
        )
    }

    pub fn submit(
        &self,
        scope: &RuntimeScope,
        artifact: ParseArtifact,
        plan_step_id: Option<String>,
    ) {
        let key = Self::key(scope);
        let mut g = self.inner.write().expect("grounding store lock");
        g.insert(
            key,
            GroundingRecord {
                artifact,
                submitted_at: Instant::now(),
                consumed: false,
                plan_step_id,
            },
        );
    }

    pub fn get_live(&self, scope: &RuntimeScope, max_age: Duration) -> Option<ParseArtifact> {
        let key = Self::key(scope);
        let g = self.inner.read().expect("grounding store lock");
        let rec = g.get(&key)?;
        if rec.consumed || rec.submitted_at.elapsed() > max_age {
            return None;
        }
        Some(rec.artifact.clone())
    }

    pub fn register_pending_postconditions(
        &self,
        scope: &RuntimeScope,
        postconditions: Vec<Postcondition>,
        tier: u8,
        cwd: Option<String>,
    ) {
        let key = Self::key(scope);
        let mut g = self.pending.write().expect("pending postconditions lock");
        g.insert(
            key,
            PendingPostconditions {
                postconditions,
                tier,
                cwd,
            },
        );
    }

    pub fn take_pending_postconditions(
        &self,
        scope: &RuntimeScope,
    ) -> Option<PendingPostconditions> {
        let key = Self::key(scope);
        let mut g = self.pending.write().expect("pending postconditions lock");
        g.remove(&key)
    }

    pub fn consume(&self, scope: &RuntimeScope) {
        let key = Self::key(scope);
        let mut g = self.inner.write().expect("grounding store lock");
        if let Some(rec) = g.get_mut(&key) {
            rec.consumed = true;
        }
    }
}
