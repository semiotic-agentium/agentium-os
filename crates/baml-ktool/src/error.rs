// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Error type for bundle parsing/auditing/unpacking.
//!
//! Deliberately independent of `baml-rt-core` so this crate can be compiled
//! into the distroless guest image. Host crates map it into their own taxonomy
//! at the call site.

/// A rejected or malformed `.ktool` bundle.
#[derive(Debug, thiserror::Error)]
pub enum KtoolError {
    /// The bundle violated the manifest/descriptor/archive contract.
    #[error("{0}")]
    Invalid(String),
    /// Filesystem failure while unpacking.
    #[error("bundle i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub type KtoolResult<T> = std::result::Result<T, KtoolError>;

pub(crate) fn invalid(message: impl Into<String>) -> KtoolError {
    KtoolError::Invalid(message.into())
}
