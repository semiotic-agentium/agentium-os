//! Session-scoped archive ref tables.
//!
//! Maps short refs (`@N`) to rendered content for the grep/paginate Read path.
//! One `RefTable` per conversation context, allocated lazily and held in the
//! `ToolSessionExecutionHandle` context map.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use dashmap::DashMap;

use crate::archive_read::{HistoryRef, RenderedContent, ShortRef};

/// A single archived tool result: rendered content + display metadata + provenance.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// YAML-rendered content, ready for grep/paginate.
    pub content: Arc<RenderedContent>,
    /// Display name of the tool that produced this result (e.g. `"support/slack"`).
    pub tool_name: String,
    /// One-line summary from `compact_result` or `describe_open`.
    pub summary: String,
    /// Number of lines in `content`.
    pub line_count: usize,
    /// Byte size of `content`.
    pub byte_count: usize,
    /// Correlates with graph `a2a_activity_anchor` / provenance activity emission for this tool result.
    /// Empty when not yet wired from the completion path.
    pub activity_anchor: String,
    /// Source kind: `"tool_result"` for archive entries.
    pub source: String,
}

impl ArchiveEntry {
    pub fn new(
        content: RenderedContent,
        tool_name: String,
        summary: String,
        activity_anchor: String,
        source: String,
    ) -> Self {
        let line_count = content.line_count();
        let byte_count = content.byte_count();
        Self {
            content: Arc::new(content),
            tool_name,
            summary,
            line_count,
            byte_count,
            activity_anchor,
            source,
        }
    }

    /// One-line display: `@3 support/slack "summary" [47 lines, 12.4KB]`
    pub fn display_header(&self, r: ShortRef) -> String {
        let kb = self.byte_count as f64 / 1024.0;
        let size_str = if kb < 1.0 {
            format!("{}B", self.byte_count)
        } else {
            format!("{:.1}KB", kb)
        };
        format!(
            r#"{} {} "{}" [{} lines, {}]"#,
            r, self.tool_name, self.summary, self.line_count, size_str
        )
    }
}

/// Identity for a `#N` history line — **no duplicated message/tool text**.
///
/// Authoritative body text for drift scoring and resolution lives in
/// [`RefTable`] under the same [`HistoryEntry::activity_anchor`] as graph `a2a_activity_anchor`
/// (see [`RefTable::insert_history`]). The LLM still sees `#N` + text in the
/// prompt; the ref table records *which provenance activity* produced the line.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// Same key as graph `a2a_activity_anchor` / core `ActivityAnchorId` for this activity.
    pub activity_anchor: String,
    /// Source kind: `"message"` or `"tool_call"`.
    pub source: String,
}

impl HistoryEntry {
    pub fn new(activity_anchor: String, source: String) -> Self {
        Self {
            activity_anchor,
            source,
        }
    }
}

/// Per-context ref table. Thread-safe: `AtomicU32` allocator, `DashMap` storage.
///
/// Both `@N` archive refs (`ShortRef`) and `#N` history refs (`HistoryRef`) share
/// the same monotonic counter so ref numbers never collide within a session context.
#[derive(Debug)]
pub struct RefTable {
    next: AtomicU32,
    entries: DashMap<u32, ArchiveEntry>,
    history: DashMap<u32, HistoryEntry>,
    /// Prompt-visible text keyed by [`HistoryEntry::activity_anchor`] (not copied on [`HistoryEntry`]).
    history_text_by_activity: DashMap<String, Arc<str>>,
}

impl RefTable {
    pub fn new() -> Self {
        Self {
            next: AtomicU32::new(1),
            entries: DashMap::new(),
            history: DashMap::new(),
            history_text_by_activity: DashMap::new(),
        }
    }
}

impl Default for RefTable {
    fn default() -> Self {
        Self::new() // Start at @1 / #1, not @0
    }
}

impl RefTable {
    /// Store an archive entry and return its allocated `ShortRef` (`@N`).
    pub fn insert(&self, entry: ArchiveEntry) -> ShortRef {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let r = ShortRef::new(n);
        self.entries.insert(n, entry);
        r
    }

    /// Store a `#N` mapping: [`HistoryEntry`] (identity) plus authoritative `content` keyed by `entry.activity_anchor`.
    ///
    /// Text is stored once per activity anchor, not on [`HistoryEntry`], so resolution and graph replay
    /// both key off the same `a2a_activity_anchor` string.
    pub fn insert_history(&self, entry: HistoryEntry, content: impl Into<Arc<str>>) -> HistoryRef {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let r = HistoryRef::new(n);
        self.history_text_by_activity
            .insert(entry.activity_anchor.clone(), content.into());
        self.history.insert(n, entry);
        r
    }

    /// Resolve the prompt/tool-call body for a history line by activity anchor (`a2a_activity_anchor`).
    pub fn history_text_for_activity(&self, activity_anchor: &str) -> Option<Arc<str>> {
        self.history_text_by_activity
            .get(activity_anchor)
            .map(|r| Arc::clone(r.value()))
    }

    /// Resolve a `ShortRef` (`@N`) to its `ArchiveEntry`, if present.
    pub fn get(&self, r: ShortRef) -> Option<dashmap::mapref::one::Ref<'_, u32, ArchiveEntry>> {
        self.entries.get(&r.as_u32())
    }

    /// Resolve a `HistoryRef` (`#N`) to its `HistoryEntry`, if present.
    pub fn get_history(
        &self,
        r: HistoryRef,
    ) -> Option<dashmap::mapref::one::Ref<'_, u32, HistoryEntry>> {
        self.history.get(&r.as_u32())
    }

    /// Insert an archive entry at an explicit `@N` index (historic replay, episodes).
    ///
    /// Does not allocate a new number; bumps the shared counter so later [`insert`](Self::insert)
    /// calls never reuse `n`.
    pub fn insert_virtual_archive(&self, n: u32, entry: ArchiveEntry) {
        debug_assert!(n > 0, "ref indices start at 1");
        self.entries.insert(n, entry);
        self.next.fetch_max(n.saturating_add(1), Ordering::Relaxed);
    }

    /// Insert a history entry at an explicit `#N` index (historic replay, episodes).
    pub fn insert_virtual_history(
        &self,
        n: u32,
        entry: HistoryEntry,
        content: impl Into<Arc<str>>,
    ) {
        debug_assert!(n > 0, "ref indices start at 1");
        self.history_text_by_activity
            .insert(entry.activity_anchor.clone(), content.into());
        self.history.insert(n, entry);
        self.next.fetch_max(n.saturating_add(1), Ordering::Relaxed);
    }
}

/// Convenience: shared `RefTable` per context, created on first use.
pub type ContextRefTables = DashMap<String, Arc<RefTable>>;

/// Get or create the `RefTable` for a context ID.
pub fn get_or_create_ref_table(tables: &ContextRefTables, context_id: &str) -> Arc<RefTable> {
    tables
        .entry(context_id.to_string())
        .or_insert_with(|| Arc::new(RefTable::new()))
        .clone()
}

/// Get the `RefTable` for a context ID if it exists; returns `None` if not yet created.
pub fn get_ref_table(tables: &ContextRefTables, context_id: &str) -> Option<Arc<RefTable>> {
    tables.get(context_id).map(|r| r.clone())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::archive_read::render_to_lines;

    fn make_entry(tool: &str, summary: &str) -> ArchiveEntry {
        let content = render_to_lines(&json!([{"name": "alice"}]));
        ArchiveEntry::new(
            content,
            tool.into(),
            summary.into(),
            String::new(),
            "tool_result".into(),
        )
    }

    #[test]
    fn insert_and_get() {
        let table = RefTable::new();
        let entry = make_entry("support/crm", "listed 1 account");
        let r = table.insert(entry);
        assert_eq!(r.as_u32(), 1);
        assert!(table.get(r).is_some());
    }

    #[test]
    fn refs_are_monotonic() {
        let table = RefTable::new();
        let r1 = table.insert(make_entry("t", "s"));
        let r2 = table.insert(make_entry("t", "s"));
        let r3 = table.insert(make_entry("t", "s"));
        assert_eq!(r1.as_u32(), 1);
        assert_eq!(r2.as_u32(), 2);
        assert_eq!(r3.as_u32(), 3);
    }

    #[test]
    fn get_unknown_ref_returns_none() {
        let table = RefTable::new();
        assert!(table.get(ShortRef::new(99)).is_none());
    }

    #[test]
    fn display_header() {
        let content = render_to_lines(&json!([{"msg": "hello"}]));
        let entry = ArchiveEntry::new(
            content,
            "support/slack".into(),
            "fetched 1 message".into(),
            String::new(),
            "tool_result".into(),
        );
        let r = ShortRef::new(3);
        let header = entry.display_header(r);
        assert!(header.starts_with("@3 support/slack"));
        assert!(header.contains("fetched 1 message"));
    }

    #[test]
    fn history_insert_and_get() {
        let table = RefTable::new();
        let entry = HistoryEntry::new("evt-001".into(), "message".into());
        let r = table.insert_history(entry, "Can you analyse our Q4 accounts?");
        assert_eq!(r.as_u32(), 1);
        assert!(table.get_history(r).is_some());
        assert_eq!(
            table.history_text_for_activity("evt-001").as_deref(),
            Some("Can you analyse our Q4 accounts?")
        );
    }

    #[test]
    fn archive_and_history_share_counter() {
        let table = RefTable::new();
        let h = table.insert_history(HistoryEntry::new("e1".into(), "message".into()), "msg");
        let a = table.insert(make_entry("t", "s"));
        let h2 = table.insert_history(HistoryEntry::new("e2".into(), "tool_call".into()), "msg2");
        assert_eq!(h.as_u32(), 1);
        assert_eq!(a.as_u32(), 2);
        assert_eq!(h2.as_u32(), 3);
        // Cross-lookup must not find entry in the wrong map.
        assert!(table.get(ShortRef::new(1)).is_none());
        assert!(
            table
                .get_history(crate::archive_read::HistoryRef::new(2))
                .is_none()
        );
    }

    #[test]
    fn context_ref_tables() {
        let tables = ContextRefTables::new();
        let t1 = get_or_create_ref_table(&tables, "ctx-1");
        let t2 = get_or_create_ref_table(&tables, "ctx-1");
        let t3 = get_or_create_ref_table(&tables, "ctx-2");
        assert!(Arc::ptr_eq(&t1, &t2));
        assert!(!Arc::ptr_eq(&t1, &t3));
    }
}
