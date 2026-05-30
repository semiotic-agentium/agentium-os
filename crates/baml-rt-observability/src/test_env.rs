// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Test helper for process-global env mutation.
//!
//! All tests in this crate that set or unset env vars acquire this shared mutex so
//! parallel test execution can't race. On drop, the scope restores the env back to what
//! it observed on entry.

use std::sync::{Mutex, MutexGuard};

static ENV_GUARD: Mutex<()> = Mutex::new(());

pub(crate) struct EnvScope {
    _guard: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvScope {
    pub(crate) fn new() -> Self {
        Self {
            _guard: ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner()),
            saved: Vec::new(),
        }
    }

    pub(crate) fn set(&mut self, key: &'static str, value: Option<&str>) {
        self.saved.push((key, std::env::var(key).ok()));
        match value {
            // SAFETY: tests acquire the ENV_GUARD mutex before constructing `EnvScope`
            // so no other test thread mutates process env concurrently.
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..).rev() {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}
