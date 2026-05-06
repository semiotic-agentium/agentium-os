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

    /// One-line display: ref + summary + size (tool name is omitted here — it appears on the
    /// session open / tool lines; keeps `@N` from duplicating next to `cat -n @N` in reads).
    pub fn display_header(&self, r: ShortRef) -> String {
        let kb = self.byte_count as f64 / 1024.0;
        let size_str = if kb < 1.0 {
            format!("{}B", self.byte_count)
        } else {
            format!("{:.1}KB", kb)
        };
        format!(
            r#"{r} · "{}" · {}L · {}"#,
            self.summary, self.line_count, size_str
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
///
/// # Idempotent history refs (`#N`) per activity
///
/// [`RefTable::insert_history`] is **idempotent** per
/// `(\`activity_anchor\`, \`source\`)` (e.g. `("evt-1", "message")`): repeated
/// full-graph [`project_prompt_context`](crate::prompt_projection::project_prompt_context)
/// passes and citation-drift reprojection **reuse** the same `HistoryRef` and do
/// not advance [`insert`] / [`insert_history`] indices for that line. Updated body
/// text for the same activity is written into [`RefTable::history_text_for_activity`]
/// on each call.
///
/// # Why `@N` for **new** archives can still be large
///
/// A [`ContextRefTables`] map is **per** `context_id` string. New archive rows use
/// [`insert`](Self::insert) which always allocates the next number. The counter is
/// **never reset** for a context.
#[derive(Debug)]
pub struct RefTable {
    next: AtomicU32,
    /// Archive bodies keyed by [`ShortRef::cell_key`] (`prefix` + `local`).
    entries: DashMap<u64, ArchiveEntry>,
    history: DashMap<u32, HistoryEntry>,
    /// `(activity_anchor, source)` (see [`history_stable_key`]) -> allocated `#N` index.
    history_stable_key_to_n: DashMap<String, u32>,
    /// Prompt-visible text keyed by [`HistoryEntry::activity_anchor`] (not copied on [`HistoryEntry`]).
    history_text_by_activity: DashMap<String, Arc<str>>,
}

#[inline]
fn history_stable_key(entry: &HistoryEntry) -> String {
    format!("{}\0{}", entry.activity_anchor, entry.source)
}

impl RefTable {
    pub fn new() -> Self {
        Self {
            next: AtomicU32::new(1),
            entries: DashMap::new(),
            history: DashMap::new(),
            history_stable_key_to_n: DashMap::new(),
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
    /// Store an archive entry and return its allocated `ShortRef` (`@N`, implicit prefix `1`).
    ///
    /// Uses the shared counter with `#N` history so legacy single-agent sessions keep prior
    /// interleaving semantics. Cluster-backed allocation should use [`Self::insert_at`] instead.
    pub fn insert(&self, entry: ArchiveEntry) -> ShortRef {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let r = ShortRef::new(n);
        self.entries.insert(r.cell_key(), entry);
        r
    }

    /// Insert at an explicit ref (e.g. Surreal-allocated `prefix` / `local`).
    pub fn insert_at(&self, archive_ref: ShortRef, entry: ArchiveEntry) {
        self.entries.insert(archive_ref.cell_key(), entry);
    }

    /// Store a `#N` mapping: [`HistoryEntry`] (identity) plus authoritative `content` keyed by `entry.activity_anchor`.
    ///
    /// Text is stored once per activity anchor, not on [`HistoryEntry`], so resolution and graph replay
    /// both key off the same `a2a_activity_anchor` string.
    ///
    /// If this `(activity_anchor, source)` was already registered (including via
    /// [`insert_virtual_history`]), returns the **existing** [`HistoryRef`] and
    /// refreshes the stored text.
    pub fn insert_history(&self, entry: HistoryEntry, content: impl Into<Arc<str>>) -> HistoryRef {
        let key = history_stable_key(&entry);
        let content: Arc<str> = content.into();
        if let Some(existing) = self.history_stable_key_to_n.get(&key) {
            let n = *existing;
            self.history_text_by_activity
                .insert(entry.activity_anchor.clone(), Arc::clone(&content));
            // Ensure the history slot exists (defensive: virtual insert may have registered key first)
            if self.history.get(&n).is_none() {
                self.history.insert(n, entry);
            }
            return HistoryRef::new(n);
        }
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        self.history_stable_key_to_n.insert(key, n);
        self.history_text_by_activity
            .insert(entry.activity_anchor.clone(), content);
        self.history.insert(n, entry);
        HistoryRef::new(n)
    }

    /// Resolve the prompt/tool-call body for a history line by activity anchor (`a2a_activity_anchor`).
    pub fn history_text_for_activity(&self, activity_anchor: &str) -> Option<Arc<str>> {
        self.history_text_by_activity
            .get(activity_anchor)
            .map(|r| Arc::clone(r.value()))
    }

    /// Resolve a `ShortRef` (`@N` or `@p/k`) to its `ArchiveEntry`, if present.
    pub fn get(&self, r: ShortRef) -> Option<dashmap::mapref::one::Ref<'_, u64, ArchiveEntry>> {
        self.entries.get(&r.cell_key())
    }

    /// Resolve a `HistoryRef` (`#N`) to its `HistoryEntry`, if present.
    pub fn get_history(
        &self,
        r: HistoryRef,
    ) -> Option<dashmap::mapref::one::Ref<'_, u32, HistoryEntry>> {
        self.history.get(&r.as_u32())
    }

    /// Insert an archive entry at an explicit legacy `@N` (prefix `1`, local `n`) for replay / episodes.
    ///
    /// Bumps the shared counter so later [`insert`](Self::insert) never reuses that local slot
    /// under prefix `1`.
    pub fn insert_virtual_archive(&self, n: u32, entry: ArchiveEntry) {
        debug_assert!(n > 0, "ref indices start at 1");
        let r = ShortRef::new(n);
        self.entries.insert(r.cell_key(), entry);
        self.next.fetch_max(n.saturating_add(1), Ordering::Relaxed);
    }

    /// Insert at a composite ref (multi-agent archive namespace).
    pub fn insert_virtual_archive_ref(&self, archive_ref: ShortRef, entry: ArchiveEntry) {
        debug_assert!(archive_ref.local > 0, "ref local indices start at 1");
        self.entries.insert(archive_ref.cell_key(), entry);
    }

    /// Insert a history entry at an explicit `#N` index (historic replay, episodes).
    pub fn insert_virtual_history(
        &self,
        n: u32,
        entry: HistoryEntry,
        content: impl Into<Arc<str>>,
    ) {
        debug_assert!(n > 0, "ref indices start at 1");
        let key = history_stable_key(&entry);
        if let Some(existing) = self.history_stable_key_to_n.get(&key) {
            assert_eq!(
                *existing, n,
                "insert_virtual_history: stable key already mapped to a different n"
            );
        } else {
            self.history_stable_key_to_n.insert(key, n);
        }
        self.history_text_by_activity
            .insert(entry.activity_anchor.clone(), content.into());
        self.history.insert(n, entry);
        self.next.fetch_max(n.saturating_add(1), Ordering::Relaxed);
    }
}

/// Convenience: shared `RefTable` per context, created on first use.
///
/// Each BAML runtime manager (QuickJS host) normally holds its own `Arc<ContextRefTables>`.
/// For multiple managers that share one provenance `context_id` (e.g. coordinator and
/// internal-A2A callee **in the same OS process**), use one [`SharedContextRefStore`] and inject
/// the same `Arc` into every manager so archive refs resolve across those managers without
/// hitting the database on every read.
pub type ContextRefTables = DashMap<String, Arc<RefTable>>;

/// Process-scoped [`ContextRefTables`] shared across several BAML runtime manager instances for
/// the same logical conversation (matching provenance `context_id`).
///
/// **Single-host only:** this is in-process RAM. It does **not** synchronize archive bodies
/// across runner pods or machines. For cluster / multi-runtime correctness, wire the Surreal-backed
/// `SurrealProvenanceStore` from the `baml-rt-provenance` crate on the runtime; this map is at most a
/// same-process cache layered on that backing.
///
/// Clone is cheap (`Arc` inner). The agent runner keeps one store per process and passes it
/// into each deployed agent's runtime build.
#[derive(Clone, Debug)]
pub struct SharedContextRefStore(Arc<ContextRefTables>);

impl SharedContextRefStore {
    pub fn new() -> Self {
        Self(Arc::new(ContextRefTables::new()))
    }

    /// Map passed to [`get_or_create_ref_table`] and tool-session archive paths.
    pub fn tables(&self) -> Arc<ContextRefTables> {
        Arc::clone(&self.0)
    }

    /// Borrow the inner map without incrementing the `Arc` reference count.
    pub fn as_ref_tables(&self) -> &ContextRefTables {
        &self.0
    }
}

impl Default for SharedContextRefStore {
    fn default() -> Self {
        Self::new()
    }
}

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
        assert!(header.starts_with("@3 · "));
        assert!(header.contains("fetched 1 message"));
        assert!(!header.contains("support/slack"));
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

    #[test]
    fn shared_context_ref_store_clones_share_arc() {
        let a = SharedContextRefStore::new();
        let b = a.clone();
        assert!(Arc::ptr_eq(&a.tables(), &b.tables()));
    }

    /// Same `(activity_anchor, source)` reuses `#N` across reprojection (no counter churn).
    #[test]
    fn insert_history_is_idempotent_for_same_activity_and_source() {
        let table = RefTable::new();
        let e1 = HistoryEntry::new("anchor-a".into(), "message".into());
        let r1 = table.insert_history(e1.clone(), "first");
        let r2 = table.insert_history(e1, "second body");
        assert_eq!(r1.as_u32(), r2.as_u32());
        let e2 = HistoryEntry::new("anchor-b".into(), "message".into());
        let r3 = table.insert_history(e2, "other");
        assert_eq!(
            r3.as_u32(),
            2,
            "only one new #N after two idempotent inserts for anchor-a"
        );
        assert_eq!(
            table.history_text_for_activity("anchor-a").as_deref(),
            Some("second body")
        );
    }
}
