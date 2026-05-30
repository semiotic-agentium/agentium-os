// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Optional lookup surface for ref-table content outside a live session (e.g. historic episodes).
//!
//! [`crate::archive_refs::RefTable`] implements [`VirtualArchiveSource`] so callers can treat
//! live and replayed tables uniformly.

use std::sync::Arc;

use crate::{
    archive_read::{HistoryRef, RenderedContent, ShortRef},
    archive_refs::RefTable,
};

/// Resolved `#N` row for tools that need message / tool-call body text.
#[derive(Debug, Clone)]
pub struct VirtualHistoryRow {
    pub activity_anchor: String,
    pub source: String,
    pub text: Arc<str>,
}

/// Resolved `@N` row for tools that need archived tool output.
#[derive(Debug, Clone)]
pub struct VirtualArchiveRow {
    pub activity_anchor: String,
    pub tool_name: String,
    pub summary: Option<String>,
    pub action_identity: Option<String>,
    pub content: Arc<RenderedContent>,
}

impl VirtualArchiveRow {
    /// Same shape as [`crate::archive_refs::ArchiveEntry::display_header`].
    #[must_use]
    pub fn display_header(&self, r: ShortRef) -> String {
        let line_count = self.content.line_count();
        let byte_count = self.content.byte_count();
        let kb = byte_count as f64 / 1024.0;
        let size_str = if kb < 1.0 {
            format!("{byte_count}B")
        } else {
            format!("{kb:.1}KB")
        };
        if let Some(action) = self.action_identity.as_deref() {
            return format!(
                "{r} · {}:{action} · {line_count}L · {size_str}",
                self.tool_name
            );
        }
        if let Some(summary) = self.summary.as_deref() {
            return format!(r#"{r} · "{summary}" · {line_count}L · {size_str}"#);
        }
        format!("{r} · {} · {line_count}L · {size_str}", self.tool_name)
    }
}

/// Read-only view of session ref data keyed by `#N` / `@N` (or `@p/k`) on the wire.
pub trait VirtualArchiveSource {
    /// History namespace (`#N`).
    fn history_row(&self, n: u32) -> Option<VirtualHistoryRow>;
    /// Archive namespace (`@N` or `@prefix/local`).
    fn archive_row(&self, archive_ref: ShortRef) -> Option<VirtualArchiveRow>;
}

impl VirtualArchiveSource for RefTable {
    fn history_row(&self, n: u32) -> Option<VirtualHistoryRow> {
        let r = HistoryRef::new(n);
        let entry = self.get_history(r)?;
        let text = self.history_text_for_activity(entry.activity_anchor.as_str())?;
        Some(VirtualHistoryRow {
            activity_anchor: entry.activity_anchor.clone(),
            source: entry.source.clone(),
            text,
        })
    }

    fn archive_row(&self, archive_ref: ShortRef) -> Option<VirtualArchiveRow> {
        let e = self.get(archive_ref)?;
        Some(VirtualArchiveRow {
            activity_anchor: e.activity_anchor.clone(),
            tool_name: e.tool_name.clone(),
            summary: e.summary.clone(),
            action_identity: e.action_identity.clone(),
            content: Arc::clone(&e.content),
        })
    }
}

impl<T: VirtualArchiveSource + ?Sized> VirtualArchiveSource for Arc<T> {
    fn history_row(&self, n: u32) -> Option<VirtualHistoryRow> {
        (**self).history_row(n)
    }

    fn archive_row(&self, archive_ref: ShortRef) -> Option<VirtualArchiveRow> {
        (**self).archive_row(archive_ref)
    }
}
