// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Value types shared by sandbox providers and the VM pool.

use std::time::SystemTime;

/// Opaque handle to a VM a provider created. The pool treats it as an
/// identifier and passes it back to [`super::SandboxProvider::teardown`].
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub name: String,
    pub created_at: SystemTime,
}

impl SandboxHandle {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            created_at: SystemTime::now(),
        }
    }
}
