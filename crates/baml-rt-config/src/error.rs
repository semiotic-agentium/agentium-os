// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Config store error type for clear boundary and HTTP mapping.
//!
//! Storage, lock, and JSON failures are distinct so API/runner can map to status codes or logging.

use baml_rt_core::BamlRtError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigStoreError {
    #[error("config store: {0}")]
    Storage(String),

    #[error("config store lock poisoned: {0}")]
    LockPoisoned(String),

    #[error("config JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<ConfigStoreError> for BamlRtError {
    fn from(e: ConfigStoreError) -> Self {
        BamlRtError::Configuration(e.to_string())
    }
}
