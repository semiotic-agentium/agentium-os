// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// A framing/protocol violation. The connection is retired.
    #[error("supervisor protocol violation: {0}")]
    Protocol(String),
    #[error("supervisor i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// Startup/configuration failure.
    #[error("supervisor configuration error: {0}")]
    Configuration(String),
}

impl From<baml_ktool::KtoolError> for SupervisorError {
    fn from(error: baml_ktool::KtoolError) -> Self {
        Self::Protocol(error.to_string())
    }
}
