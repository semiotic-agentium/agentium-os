// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStatus {
    Resolved,
    Unresolved,
    Negated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationIntegrityEntry {
    pub raw: String,
    pub n: u32,
    pub is_history: bool,
    pub negated: bool,
    pub status: IntegrityStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationIntegrityAssessment {
    pub per_citation: Vec<CitationIntegrityEntry>,
    pub unresolved_count: u32,
    pub resolved_count: u32,
    #[serde(default)]
    pub strict_mode: bool,
    #[serde(default)]
    pub strict_violation: bool,
}

impl CitationIntegrityAssessment {
    pub fn severity(&self, strict_citation_anchors: bool) -> &'static str {
        if self.unresolved_count > 0 {
            if strict_citation_anchors {
                "error"
            } else {
                "warn"
            }
        } else {
            "acceptable"
        }
    }
}
