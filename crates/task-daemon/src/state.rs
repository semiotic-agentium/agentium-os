// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Persistent daemon state (cursor, channel resolution, dedupe keys).

use std::{
    collections::BTreeMap,
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
/// Stable key for ClickUp lifecycle revision counters persisted in daemon state.
pub struct ClickupLifecycleRevisionSlot(String);

impl ClickupLifecycleRevisionSlot {
    pub fn new(slot: String) -> Self {
        Self(slot)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Snapshot of a ClickUp task tracked for reconciliation.
pub struct ClickupTaskSnapshot {
    pub list_id: String,
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Per-source persisted state.
pub struct SourceState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfill_latest_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_last_seen_ts: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen_task_keys: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clickup_task_snapshot: BTreeMap<String, ClickupTaskSnapshot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clickup_lifecycle_revisions: BTreeMap<ClickupLifecycleRevisionSlot, u64>,
}

impl SourceState {
    /// Returns true when a derived task key has already been delivered.
    pub fn has_seen_task(&self, task_key: &str) -> bool {
        self.seen_task_keys.contains_key(task_key)
    }

    /// Marks a derived task key as delivered.
    pub fn mark_task_seen(&mut self, task_key: String, seen_at_unix: u64) {
        self.seen_task_keys.insert(task_key, seen_at_unix);
    }

    /// Trims dedupe memory to the newest `max_entries` keys.
    pub fn prune_seen_tasks(&mut self, max_entries: usize) {
        if self.seen_task_keys.len() <= max_entries {
            return;
        }

        let mut by_age: Vec<(String, u64)> = self
            .seen_task_keys
            .iter()
            .map(|(key, seen_at)| (key.clone(), *seen_at))
            .collect();
        by_age.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

        let remove_count = by_age.len().saturating_sub(max_entries);
        for (key, _) in by_age.into_iter().take(remove_count) {
            self.seen_task_keys.remove(&key);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Top-level persisted daemon state across all sources.
pub struct TaskDaemonState {
    pub version: u8,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, SourceState>,
}

impl Default for TaskDaemonState {
    fn default() -> Self {
        Self {
            version: 1,
            sources: BTreeMap::new(),
        }
    }
}

impl TaskDaemonState {
    /// Returns mutable state for a source, creating it if missing.
    pub fn source_state_mut(&mut self, source_key: &str) -> &mut SourceState {
        self.sources.entry(source_key.to_string()).or_default()
    }

    /// Returns state for a source if it exists.
    pub fn source_state(&self, source_key: &str) -> Option<&SourceState> {
        self.sources.get(source_key)
    }
}

#[derive(Debug, Clone)]
/// Filesystem-backed state store with atomic writes.
pub struct StateStore {
    path: PathBuf,
    pub max_seen_tasks_per_source: usize,
}

impl StateStore {
    /// Creates a state store at `path`.
    pub fn new(path: PathBuf, max_seen_tasks_per_source: usize) -> Self {
        Self {
            path,
            max_seen_tasks_per_source,
        }
    }

    /// Returns the on-disk state file path.
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Loads state from disk, returning defaults when no file exists.
    ///
    /// The filesystem read runs on [`tokio::task::spawn_blocking`] so the
    /// daemon poll loop's executor worker stays free to drive other
    /// futures while disk I/O is in flight.
    ///
    /// A scheduled load cannot be cancelled — the blocking pool runs the
    /// read to completion even if the awaiting future is dropped.
    pub async fn load(&self) -> Result<TaskDaemonState> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || load_blocking(&path))
            .await
            .map_err(propagate_join_error("daemon state load"))?
    }

    /// Persists state to disk via write-then-rename atomic replacement.
    ///
    /// The serialize + `sync_all` + rename chain runs on
    /// [`tokio::task::spawn_blocking`]. The caller's `&TaskDaemonState`
    /// is cloned once into the blocking task so the calling future
    /// retains ownership and the executor worker is not blocked on
    /// `fsync`.
    ///
    /// A scheduled save cannot be cancelled — the rename completes even
    /// if the calling future is dropped. This is desirable for crash
    /// safety: a half-written `state.json` is never visible to a future
    /// `load()`.
    pub async fn save(&self, state: &TaskDaemonState) -> Result<()> {
        let path = self.path.clone();
        let max = self.max_seen_tasks_per_source;
        let snapshot = state.clone();
        tokio::task::spawn_blocking(move || save_blocking(&path, max, snapshot))
            .await
            .map_err(propagate_join_error("daemon state save"))?
    }
}

/// Maps a `JoinError` to an `anyhow::Error` for non-panic outcomes, but
/// re-raises panics so a bug inside the blocking task aborts the daemon
/// task loudly instead of being downgraded to a transient-looking error
/// the poll loop would retry indefinitely.
fn propagate_join_error(
    operation: &'static str,
) -> impl FnOnce(tokio::task::JoinError) -> anyhow::Error {
    move |e| {
        if e.is_panic() {
            std::panic::resume_unwind(e.into_panic());
        }
        anyhow::anyhow!("{operation} blocking task failed: {e}")
    }
}

fn load_blocking(path: &Path) -> Result<TaskDaemonState> {
    // Match on the read error directly instead of pre-checking with
    // `path.exists()` — a concurrent rename-onto-target by a peer save
    // can otherwise flip the outcome from "missing file → defaults" to
    // a hard read error.
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(TaskDaemonState::default()),
        Err(e) => {
            return Err(e).with_context(|| format!("reading daemon state at {}", path.display()));
        }
    };
    let state: TaskDaemonState = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing daemon state at {}", path.display()))?;
    Ok(state)
}

fn save_blocking(
    path: &Path,
    max_seen_tasks_per_source: usize,
    mut state: TaskDaemonState,
) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating state directory for {}", path.display()))?;
    }

    for source in state.sources.values_mut() {
        source.prune_seen_tasks(max_seen_tasks_per_source);
    }

    let payload =
        serde_json::to_vec_pretty(&state).context("serializing task daemon state to JSON")?;
    let tmp = unique_temp_path(path);

    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("creating temporary state file {}", tmp.display()))?;
        file.write_all(&payload)
            .with_context(|| format!("writing temporary state file {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing temporary state file {}", tmp.display()))?;
    }

    fs::rename(&tmp, path).with_context(|| {
        format!(
            "atomically replacing daemon state {} with {}",
            path.display(),
            tmp.display()
        )
    })?;
    Ok(())
}

/// Produces a per-call unique temp-file path next to `path` so two
/// concurrent saves (e.g. from cloned `StateStore` handles) cannot
/// collide on a single `state.json.tmp` and corrupt each other's
/// rename-into-place sequence.
fn unique_temp_path(path: &Path) -> PathBuf {
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("{pid}-{nanos}-{seq}.tmp"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn prune_seen_tasks_keeps_newest_entries() {
        let mut source_state = SourceState::default();
        source_state.mark_task_seen("task-a".to_string(), 10);
        source_state.mark_task_seen("task-b".to_string(), 30);
        source_state.mark_task_seen("task-c".to_string(), 20);

        source_state.prune_seen_tasks(2);

        assert!(!source_state.has_seen_task("task-a"));
        assert!(source_state.has_seen_task("task-b"));
        assert!(source_state.has_seen_task("task-c"));
    }

    #[tokio::test]
    async fn load_returns_default_when_file_missing() {
        let dir = tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("missing.json"), 100);

        let state = store.load().await.expect("load");

        assert_eq!(state.version, 1);
        assert!(state.sources.is_empty());
    }

    #[tokio::test]
    async fn save_then_load_roundtrips_state() {
        let dir = tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("state.json"), 100);

        let mut original = TaskDaemonState::default();
        original
            .source_state_mut("slack:C123")
            .mark_task_seen("task-1".to_string(), 42);

        store.save(&original).await.expect("save");
        let loaded = store.load().await.expect("load");

        let source = loaded
            .source_state("slack:C123")
            .expect("source state should round-trip");
        assert_eq!(
            source.seen_task_keys.get("task-1"),
            Some(&42),
            "seen-task timestamp should round-trip verbatim"
        );
    }

    /// Sanity check that load + save compose under a `current_thread`
    /// runtime — the spawn_blocking pattern delegates disk I/O to the
    /// blocking-thread pool (separate from the runtime worker), so the
    /// single executor thread is free to drive the awaiting future. A
    /// future regression that reverted to inline `fs::*` calls would
    /// still pass this test (sync I/O completes synchronously on the
    /// worker), but the test guards against the cheaper class of bug:
    /// accidentally introducing a runtime-on-runtime construction
    /// (e.g. `Runtime::new()`/`block_on` inside the async path) which
    /// panics under `flavor = "current_thread"` because there is no
    /// blocking pool to relocate the inner runtime onto.
    #[tokio::test(flavor = "current_thread")]
    async fn load_and_save_compose_under_current_thread_runtime() {
        let dir = tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("state.json"), 100);

        let mut state = TaskDaemonState::default();
        state
            .source_state_mut("source-1")
            .mark_task_seen("task-1".to_string(), 1);
        store.save(&state).await.expect("save");
        let loaded = store.load().await.expect("load");
        assert!(
            loaded
                .source_state("source-1")
                .unwrap()
                .has_seen_task("task-1")
        );
    }
}
