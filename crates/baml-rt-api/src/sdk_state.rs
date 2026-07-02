// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! In-process SDK state (eval sessions) wired through [`ApiState`](crate::router::ApiState).

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use crate::eval_handlers::EvalSessionSpec;

#[derive(Debug, Default)]
pub struct EvalSessionStore {
    inner: RwLock<HashMap<String, EvalSessionSpec>>,
}

impl EvalSessionStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn insert(&self, id: String, spec: EvalSessionSpec) -> Result<(), EvalSessionStoreError> {
        self.inner
            .write()
            .map_err(|_| EvalSessionStoreError)?
            .insert(id, spec);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<EvalSessionSpec> {
        self.inner.read().ok()?.get(id).cloned()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EvalSessionStoreError;

impl std::fmt::Display for EvalSessionStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("eval session store poisoned")
    }
}

impl std::error::Error for EvalSessionStoreError {}
