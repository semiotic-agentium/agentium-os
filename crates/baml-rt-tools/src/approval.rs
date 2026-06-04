// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    #[default]
    Pending,
    Approved,
    Rejected,
    Stale,
}

impl ApprovalState {
    pub fn is_approved(self) -> bool {
        matches!(self, Self::Approved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord<S = ApprovalState> {
    pub state: S,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl<S: Default> ApprovalRecord<S> {
    pub fn pending() -> Self {
        Self {
            state: S::default(),
            owner: None,
            reviewed_at: None,
            expires_at: None,
        }
    }
}
