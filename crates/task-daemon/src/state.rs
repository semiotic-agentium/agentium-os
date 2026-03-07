//! Persistent daemon state (cursor, channel resolution, dedupe keys).

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    pub clickup_lifecycle_revisions: BTreeMap<String, u64>,
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
    pub fn load(&self) -> Result<TaskDaemonState> {
        if !self.path.exists() {
            return Ok(TaskDaemonState::default());
        }

        let bytes = fs::read(&self.path)
            .with_context(|| format!("reading daemon state at {}", self.path.display()))?;
        let state: TaskDaemonState = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing daemon state at {}", self.path.display()))?;
        Ok(state)
    }

    /// Persists state to disk via write-then-rename atomic replacement.
    pub fn save(&self, state: &TaskDaemonState) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating state directory for {}", self.path.display()))?;
        }

        let mut normalized = state.clone();
        for source in normalized.sources.values_mut() {
            source.prune_seen_tasks(self.max_seen_tasks_per_source);
        }

        let payload = serde_json::to_vec_pretty(&normalized)
            .context("serializing task daemon state to JSON")?;
        let tmp = self.path.with_extension("json.tmp");

        {
            let mut file = fs::File::create(&tmp)
                .with_context(|| format!("creating temporary state file {}", tmp.display()))?;
            file.write_all(&payload)
                .with_context(|| format!("writing temporary state file {}", tmp.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing temporary state file {}", tmp.display()))?;
        }

        fs::rename(&tmp, &self.path).with_context(|| {
            format!(
                "atomically replacing daemon state {} with {}",
                self.path.display(),
                tmp.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}
