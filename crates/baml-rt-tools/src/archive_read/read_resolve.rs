//! Resolve `@N` for Read steps and history projection: live [`ContextRefTables`] first, then virtual replay.

use std::sync::Arc;

use crate::{
    archive_read::{RenderedContent, ShortRef, virtual_source::VirtualArchiveSource},
    archive_refs::{ContextRefTables, get_ref_table},
};

/// Resolved archive body and display header for grep / cat-n.
#[derive(Debug, Clone)]
pub struct ResolvedArchiveRead {
    content: Arc<RenderedContent>,
    header: String,
}

impl ResolvedArchiveRead {
    #[must_use]
    pub fn content(&self) -> &RenderedContent {
        &self.content
    }

    #[must_use]
    pub fn header_line(&self) -> &str {
        &self.header
    }
}

/// Prefer a live ref table row for `context_id` when present and `archive_ref` resolves; otherwise
/// use `virtual_fallback` (e.g. episode replay [`RefTable`](crate::archive_refs::RefTable)).
#[must_use]
pub fn resolve_archive_for_read(
    tables: Option<&ContextRefTables>,
    context_id: &str,
    archive_ref: ShortRef,
    virtual_fallback: Option<&dyn VirtualArchiveSource>,
) -> Option<ResolvedArchiveRead> {
    if let Some(tables) = tables
        && let Some(table) = get_ref_table(tables, context_id)
        && let Some(entry) = table.get(archive_ref)
    {
        return Some(ResolvedArchiveRead {
            content: Arc::clone(&entry.content),
            header: entry.display_header(archive_ref),
        });
    }
    let v = virtual_fallback?;
    let row = v.archive_row(archive_ref.as_u32())?;
    Some(ResolvedArchiveRead {
        content: Arc::clone(&row.content),
        header: row.display_header(archive_ref),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        archive_read::render_to_lines,
        archive_refs::{ArchiveEntry, RefTable},
    };

    fn sample_entry() -> ArchiveEntry {
        let content = render_to_lines(&json!({"k": "v"}));
        ArchiveEntry::new(
            content,
            "t/tool".into(),
            "summary".into(),
            String::new(),
            "tool_result".into(),
        )
    }

    #[test]
    fn virtual_when_context_has_no_table() {
        let tables = ContextRefTables::new();
        let vt = RefTable::new();
        vt.insert_virtual_archive(1, sample_entry());
        let r = resolve_archive_for_read(Some(&tables), "ctx-unknown", ShortRef::new(1), Some(&vt));
        assert!(r.is_some());
    }

    #[test]
    fn virtual_when_live_table_misses_ref() {
        let tables = ContextRefTables::new();
        let _ = crate::archive_refs::get_or_create_ref_table(&tables, "ctx-1");
        let vt = RefTable::new();
        vt.insert_virtual_archive(2, sample_entry());
        let r = resolve_archive_for_read(Some(&tables), "ctx-1", ShortRef::new(2), Some(&vt));
        assert!(r.is_some());
        assert!(r.unwrap().header_line().contains("@2"));
    }

    #[test]
    fn live_wins_when_ref_present() {
        let tables = ContextRefTables::new();
        let live = crate::archive_refs::get_or_create_ref_table(&tables, "ctx-1");
        let inserted = live.insert(sample_entry());
        assert_eq!(inserted.as_u32(), 1);
        let vt = RefTable::new();
        vt.insert_virtual_archive(
            1,
            ArchiveEntry::new(
                render_to_lines(&json!("other")),
                "other".into(),
                "x".into(),
                String::new(),
                "tool_result".into(),
            ),
        );
        let r = resolve_archive_for_read(Some(&tables), "ctx-1", ShortRef::new(1), Some(&vt));
        assert!(r.unwrap().header_line().contains("t/tool"));
    }
}
