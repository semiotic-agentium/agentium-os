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

use crate::archive_read::{RenderedContent, ShortRef};

/// A single archived tool result: rendered content + display metadata.
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
}

impl ArchiveEntry {
    pub fn new(content: RenderedContent, tool_name: String, summary: String) -> Self {
        let line_count = content.line_count();
        let byte_count = content.byte_count();
        Self {
            content: Arc::new(content),
            tool_name,
            summary,
            line_count,
            byte_count,
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

/// Per-context ref table. Thread-safe: `AtomicU32` allocator, `DashMap` storage.
#[derive(Debug)]
pub struct RefTable {
    next: AtomicU32,
    entries: DashMap<u32, ArchiveEntry>,
}

impl RefTable {
    pub fn new() -> Self {
        Self {
            next: AtomicU32::new(1),
            entries: DashMap::new(),
        }
    }
}

impl Default for RefTable {
    fn default() -> Self {
        Self::new() // Start at @1, not @0
    }
}

impl RefTable {
    /// Store an archive entry and return its allocated `ShortRef`.
    pub fn insert(&self, entry: ArchiveEntry) -> ShortRef {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let r = ShortRef::new(n);
        self.entries.insert(n, entry);
        r
    }

    /// Resolve a `ShortRef` to its `ArchiveEntry`, if present.
    pub fn get(&self, r: ShortRef) -> Option<dashmap::mapref::one::Ref<'_, u32, ArchiveEntry>> {
        self.entries.get(&r.as_u32())
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

    #[test]
    fn insert_and_get() {
        let table = RefTable::new();
        let content = render_to_lines(&json!([{"name": "alice"}]));
        let entry = ArchiveEntry::new(content, "support/crm".into(), "listed 1 account".into());
        let r = table.insert(entry);
        assert_eq!(r.as_u32(), 1);
        assert!(table.get(r).is_some());
    }

    #[test]
    fn refs_are_monotonic() {
        let table = RefTable::new();
        let content = render_to_lines(&json!({"x": 1}));
        let r1 = table.insert(ArchiveEntry::new(content.clone(), "t".into(), "s".into()));
        let r2 = table.insert(ArchiveEntry::new(content.clone(), "t".into(), "s".into()));
        let r3 = table.insert(ArchiveEntry::new(content, "t".into(), "s".into()));
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
        let entry = ArchiveEntry::new(content, "support/slack".into(), "fetched 1 message".into());
        let r = ShortRef::new(3);
        let header = entry.display_header(r);
        assert!(header.starts_with("@3 support/slack"));
        assert!(header.contains("fetched 1 message"));
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
