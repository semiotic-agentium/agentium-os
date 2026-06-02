// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! ClickUp list polling and lifecycle diffing for the task-daemon substrate.
//!
//! The host runner does not poll ClickUp directly; task-daemon calls this module and
//! publishes `host.source-records.v1` via `POST /events/publish`.

use std::{collections::BTreeMap, fmt};

use anyhow::{Context, Result};
use integrations_clickup_client::ClickUpClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::source_records::{
    CLICKUP_LIFECYCLE_EVENT_KIND, ClickupLifecycleEventRecord, clickup_previous_snapshot_value,
    clickup_task_snapshot_value,
};

const MAX_CLICKUP_LIST_TASK_PAGES: u32 = 50;

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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
/// Snapshot of a ClickUp task tracked for reconciliation.
pub struct ClickupTaskSnapshot {
    pub list_id: String,
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Per-source ClickUp poll state persisted by task-daemon.
pub struct ClickupPollState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub task_snapshot: BTreeMap<String, ClickupTaskSnapshot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lifecycle_revisions: BTreeMap<ClickupLifecycleRevisionSlot, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Priority tier for inferred lifecycle tasks (maps to task-daemon `TaskConfidence`).
pub enum ClickupInferredPriority {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone)]
/// Output of one ClickUp poll cycle.
pub struct ClickupPollOutcome {
    pub source_key: String,
    pub source_label: String,
    pub lifecycle_events: Vec<ClickupLifecycleEventRecord>,
    pub items_scanned: usize,
    pub state: ClickupPollState,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ClickupListId(String);

impl ClickupListId {
    fn parse(raw: &str) -> Option<Self> {
        let normalized = raw.trim();
        if normalized.is_empty() {
            None
        } else {
            Some(Self(normalized.to_string()))
        }
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClickupListId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickupLifecycleEventKind {
    Created,
    Terminal,
    Removed,
}

impl ClickupLifecycleEventKind {
    fn key_prefix(self) -> &'static str {
        match self {
            Self::Created => "clickup-created",
            Self::Terminal => "clickup-terminal",
            Self::Removed => "clickup-removed",
        }
    }

    fn revision_counter_prefix(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Terminal => "terminal",
            Self::Removed => "removed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClickupLifecycleEventKey {
    kind: ClickupLifecycleEventKind,
    task_id: String,
    terminal_status_slug: Option<String>,
    revision: u64,
}

impl ClickupLifecycleEventKey {
    fn created(task_id: &str) -> Self {
        Self {
            kind: ClickupLifecycleEventKind::Created,
            task_id: task_id.to_string(),
            terminal_status_slug: None,
            revision: 1,
        }
    }

    fn terminal(task_id: &str, status: &ClickupStatus) -> Self {
        Self {
            kind: ClickupLifecycleEventKind::Terminal,
            task_id: task_id.to_string(),
            terminal_status_slug: Some(status.slug()),
            revision: 1,
        }
    }

    fn removed(task_id: &str) -> Self {
        Self {
            kind: ClickupLifecycleEventKind::Removed,
            task_id: task_id.to_string(),
            terminal_status_slug: None,
            revision: 1,
        }
    }

    fn with_revision(mut self, revision: u64) -> Self {
        self.revision = revision.max(1);
        self
    }

    fn as_task_key(&self) -> String {
        let base = match self.kind {
            ClickupLifecycleEventKind::Created | ClickupLifecycleEventKind::Removed => {
                format!("{}:{}", self.kind.key_prefix(), self.task_id)
            }
            ClickupLifecycleEventKind::Terminal => {
                let slug = self.terminal_status_slug.as_deref().unwrap_or("unknown");
                format!("{}:{}:{}", self.kind.key_prefix(), self.task_id, slug)
            }
        };
        if self.revision > 1 {
            format!("{base}:r{}", self.revision)
        } else {
            base
        }
    }
}

fn next_lifecycle_revision(
    revisions: &mut BTreeMap<ClickupLifecycleRevisionSlot, u64>,
    kind: ClickupLifecycleEventKind,
    task_id: &str,
) -> u64 {
    let slot =
        ClickupLifecycleRevisionSlot::new(format!("{}:{task_id}", kind.revision_counter_prefix()));
    let next = revisions.get(&slot).copied().unwrap_or(0).saturating_add(1);
    revisions.insert(slot, next);
    next
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickupStatusClass {
    Terminal,
    NonTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClickupStatus {
    original: String,
    normalized: String,
    class: ClickupStatusClass,
}

impl ClickupStatus {
    fn from_optional(raw: Option<&str>) -> Self {
        let original = raw
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
            .to_string();
        let normalized = Self::normalize(&original);
        let class = classify_normalized_status(&normalized);
        Self {
            original,
            normalized,
            class,
        }
    }

    fn from_snapshot(raw: &str) -> Self {
        Self::from_optional(Some(raw))
    }

    fn normalize(raw: &str) -> String {
        raw.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
    }

    fn is_terminal(&self) -> bool {
        matches!(self.class, ClickupStatusClass::Terminal)
    }

    fn changed_from(&self, previous: &Self) -> bool {
        self.normalized != previous.normalized
    }

    fn slug(&self) -> String {
        self.normalized.replace(' ', "-")
    }
}

fn classify_normalized_status(status: &str) -> ClickupStatusClass {
    if status.contains("cancel")
        || status.contains("closed")
        || status.contains("complete")
        || status.contains("done")
        || status.contains("resolved")
    {
        ClickupStatusClass::Terminal
    } else {
        ClickupStatusClass::NonTerminal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickupPriority {
    Urgent,
    High,
    Normal,
    Low,
    Unknown,
}

impl ClickupPriority {
    fn from_optional(raw: Option<&str>) -> Self {
        let normalized = raw
            .map(ClickupStatus::normalize)
            .unwrap_or_default()
            .replace(' ', "");
        match normalized.as_str() {
            "1" | "urgent" => Self::Urgent,
            "2" | "high" => Self::High,
            "4" | "low" => Self::Low,
            "3" | "normal" => Self::Normal,
            _ => Self::Unknown,
        }
    }
}

impl From<ClickupPriority> for ClickupInferredPriority {
    fn from(value: ClickupPriority) -> Self {
        match value {
            ClickupPriority::Urgent | ClickupPriority::High => ClickupInferredPriority::High,
            ClickupPriority::Low => ClickupInferredPriority::Low,
            ClickupPriority::Normal | ClickupPriority::Unknown => ClickupInferredPriority::Medium,
        }
    }
}

#[derive(Debug, Error)]
/// Typed configuration failures for ClickUp polling.
pub enum ClickupSourceConfigError {
    #[error("clickup source requires at least one list id")]
    MissingListIds,
}

#[derive(Debug, Clone)]
/// Runtime configuration for ClickUp polling.
pub struct ClickupSourceConfig {
    pub list_ids: Vec<String>,
}

impl ClickupSourceConfig {
    fn normalized_list_id_values(
        &self,
    ) -> std::result::Result<Vec<ClickupListId>, ClickupSourceConfigError> {
        let mut list_ids: Vec<ClickupListId> = self
            .list_ids
            .iter()
            .filter_map(|value| ClickupListId::parse(value))
            .collect();
        list_ids.sort();
        list_ids.dedup();
        if list_ids.is_empty() {
            return Err(ClickupSourceConfigError::MissingListIds);
        }
        Ok(list_ids)
    }

    /// Normalized list ids for persistence keys and polling.
    pub fn normalized_list_ids(
        &self,
    ) -> std::result::Result<Vec<String>, ClickupSourceConfigError> {
        Ok(self
            .normalized_list_id_values()?
            .iter()
            .map(|id| id.as_str().to_string())
            .collect())
    }

    /// Stable source key for a set of monitored lists.
    pub fn source_key(list_ids: &[String]) -> String {
        let mut normalized: Vec<String> = list_ids
            .iter()
            .filter_map(|value| ClickupListId::parse(value))
            .map(|id| id.as_str().to_string())
            .collect();
        normalized.sort();
        normalized.dedup();
        format!("clickup:{}", normalized.join(","))
    }

    /// Human-readable label for operator logs.
    pub fn source_label(list_ids: &[String]) -> String {
        let parsed: Vec<ClickupListId> = list_ids
            .iter()
            .filter_map(|value| ClickupListId::parse(value))
            .collect();
        if parsed.len() == 1 {
            return format!("clickup:list:{}", parsed[0]);
        }
        format!(
            "clickup:lists:{}",
            parsed
                .iter()
                .map(ClickupListId::as_str)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

/// Poll monitored ClickUp lists and diff lifecycle events against persisted state.
pub async fn poll_clickup_lists(
    client: &ClickUpClient,
    config: &ClickupSourceConfig,
    previous: ClickupPollState,
) -> Result<ClickupPollOutcome> {
    let list_ids = config.normalized_list_id_values()?;
    let list_id_strings: Vec<String> = list_ids.iter().map(|id| id.as_str().to_string()).collect();
    let source_key = ClickupSourceConfig::source_key(&list_id_strings);
    let source_label = ClickupSourceConfig::source_label(&list_id_strings);

    let api_key = client
        .resolve_api_key()
        .context("resolve CLICKUP_API_KEY for ClickUp poll")?;

    let previous_snapshot = previous.task_snapshot;
    let mut lifecycle_revisions = previous.lifecycle_revisions;

    let mut current_snapshot: BTreeMap<String, ClickupTaskSnapshot> = BTreeMap::new();
    let mut current_records: BTreeMap<String, ClickupTaskRecord> = BTreeMap::new();

    for list_id in &list_ids {
        for task in fetch_list_tasks(client, api_key.as_str(), list_id).await? {
            current_snapshot.insert(
                task.id.clone(),
                ClickupTaskSnapshot {
                    list_id: task.list_id.as_str().to_string(),
                    name: task.name.clone(),
                    status: task.status.original.clone(),
                    url: task.url.clone(),
                },
            );
            current_records.insert(task.id.clone(), task);
        }
    }

    let mut lifecycle_events = Vec::new();

    for (task_id, task) in &current_records {
        if !previous_snapshot.contains_key(task_id) {
            let revision = next_lifecycle_revision(
                &mut lifecycle_revisions,
                ClickupLifecycleEventKind::Created,
                task_id,
            );
            lifecycle_events.push(created_lifecycle_event(task, revision));
            continue;
        }

        if let Some(previous) = previous_snapshot.get(task_id) {
            let previous_status = ClickupStatus::from_snapshot(&previous.status);
            if !previous_status.is_terminal()
                && task.status.is_terminal()
                && task.status.changed_from(&previous_status)
            {
                let revision = next_lifecycle_revision(
                    &mut lifecycle_revisions,
                    ClickupLifecycleEventKind::Terminal,
                    task_id,
                );
                lifecycle_events.push(terminal_lifecycle_event(task, previous, revision));
            }
        }
    }

    for (task_id, previous) in &previous_snapshot {
        if !current_records.contains_key(task_id) {
            let revision = next_lifecycle_revision(
                &mut lifecycle_revisions,
                ClickupLifecycleEventKind::Removed,
                task_id,
            );
            lifecycle_events.push(removed_lifecycle_event(task_id, previous, revision));
        }
    }

    Ok(ClickupPollOutcome {
        source_key,
        source_label,
        items_scanned: current_records.len(),
        lifecycle_events,
        state: ClickupPollState {
            task_snapshot: current_snapshot,
            lifecycle_revisions,
        },
    })
}

async fn fetch_list_tasks(
    client: &ClickUpClient,
    api_key: &str,
    list_id: &ClickupListId,
) -> Result<Vec<ClickupTaskRecord>> {
    let mut out = Vec::new();
    let mut page = 0_u32;
    let mut previous_page_signature: Option<Vec<String>> = None;

    loop {
        let page_value = page.to_string();
        let json = client
            .send_json(
                client
                    .get(&format!("/list/{list_id}/task"), api_key)
                    .query(&[
                        ("include_closed", "true"),
                        ("subtasks", "true"),
                        ("page", page_value.as_str()),
                    ]),
            )
            .await
            .with_context(|| {
                format!("fetching ClickUp list tasks for list {list_id} page {page}")
            })?;

        let raw = serde_json::from_value::<RawTaskList>(json).with_context(|| {
            format!("parsing ClickUp list response for list {list_id} page {page}")
        })?;

        let raw_page_len = raw.tasks.len();
        let page_signature = raw_page_signature(&raw.tasks);
        let mut page_tasks = Vec::with_capacity(raw_page_len);
        for (index, task_value) in raw.tasks.into_iter().enumerate() {
            match serde_json::from_value::<RawTask>(task_value) {
                Ok(task) => page_tasks.push(ClickupTaskRecord::from_raw(task, list_id)),
                Err(err) => {
                    tracing::warn!(
                        list_id = list_id.as_str(),
                        page,
                        task_index = index,
                        error = %err,
                        "skipping malformed ClickUp task entry"
                    );
                }
            }
        }

        if page > 0
            && previous_page_signature
                .as_ref()
                .is_some_and(|previous| previous == &page_signature)
        {
            tracing::warn!(
                list_id = list_id.as_str(),
                page,
                "ClickUp task pagination returned a duplicate page; stopping pagination"
            );
            break;
        }

        let reached_last_page = raw.last_page.unwrap_or(false);
        out.extend(page_tasks);

        if reached_last_page || raw_page_len == 0 {
            break;
        }
        if page + 1 >= MAX_CLICKUP_LIST_TASK_PAGES {
            tracing::warn!(
                list_id = list_id.as_str(),
                max_pages = MAX_CLICKUP_LIST_TASK_PAGES,
                "stopping ClickUp task pagination at safety limit"
            );
            break;
        }

        previous_page_signature = Some(page_signature);
        page = page.saturating_add(1);
    }

    Ok(out)
}

fn raw_page_signature(tasks: &[Value]) -> Vec<String> {
    tasks.iter().map(Value::to_string).collect()
}

#[derive(Debug, Clone)]
struct ClickupTaskRecord {
    id: String,
    list_id: ClickupListId,
    name: String,
    status: ClickupStatus,
    description: Option<String>,
    url: Option<String>,
    priority: ClickupPriority,
}

impl ClickupTaskRecord {
    fn from_raw(raw: RawTask, list_id: &ClickupListId) -> Self {
        let id = raw.id;
        let name = raw
            .name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("ClickUp task {id}"));
        let status =
            ClickupStatus::from_optional(raw.status.as_ref().map(|value| value.status.as_str()));
        let description = raw
            .description
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let url = raw
            .url
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let priority = ClickupPriority::from_optional(
            raw.priority
                .as_ref()
                .and_then(|value| value.priority.as_deref()),
        );

        Self {
            id,
            list_id: list_id.clone(),
            name,
            status,
            description,
            url,
            priority,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawTaskList {
    #[serde(default)]
    tasks: Vec<Value>,
    #[serde(default)]
    last_page: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawTask {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<RawTaskStatus>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    priority: Option<RawTaskPriority>,
}

#[derive(Debug, Deserialize)]
struct RawTaskStatus {
    status: String,
}

#[derive(Debug, Deserialize)]
struct RawTaskPriority {
    #[serde(default)]
    priority: Option<String>,
}

fn task_priority_wire(priority: ClickupPriority) -> Option<String> {
    Some(
        match priority {
            ClickupPriority::Urgent => "urgent",
            ClickupPriority::High => "high",
            ClickupPriority::Normal => "normal",
            ClickupPriority::Low => "low",
            ClickupPriority::Unknown => "unknown",
        }
        .to_string(),
    )
}

fn created_lifecycle_event(task: &ClickupTaskRecord, revision: u64) -> ClickupLifecycleEventRecord {
    let key = ClickupLifecycleEventKey::created(&task.id)
        .with_revision(revision)
        .as_task_key();
    ClickupLifecycleEventRecord {
        record_kind: CLICKUP_LIFECYCLE_EVENT_KIND.to_string(),
        key,
        event: "created".to_string(),
        task_id: task.id.clone(),
        list_id: task.list_id.as_str().to_string(),
        revision,
        snapshot: clickup_task_snapshot_value(
            &task.id,
            task.list_id.as_str(),
            &task.name,
            &task.status.original,
            task.description.as_deref(),
            task.url.as_deref(),
            task_priority_wire(task.priority).as_deref(),
        ),
        previous_snapshot: None,
    }
}

fn terminal_lifecycle_event(
    task: &ClickupTaskRecord,
    previous: &ClickupTaskSnapshot,
    revision: u64,
) -> ClickupLifecycleEventRecord {
    let key = ClickupLifecycleEventKey::terminal(&task.id, &task.status)
        .with_revision(revision)
        .as_task_key();
    ClickupLifecycleEventRecord {
        record_kind: CLICKUP_LIFECYCLE_EVENT_KIND.to_string(),
        key,
        event: "terminal".to_string(),
        task_id: task.id.clone(),
        list_id: task.list_id.as_str().to_string(),
        revision,
        snapshot: clickup_task_snapshot_value(
            &task.id,
            task.list_id.as_str(),
            &task.name,
            &task.status.original,
            task.description.as_deref(),
            task.url.as_deref(),
            task_priority_wire(task.priority).as_deref(),
        ),
        previous_snapshot: Some(clickup_previous_snapshot_value(
            &previous.list_id,
            &previous.name,
            &previous.status,
            previous.url.as_deref(),
        )),
    }
}

fn removed_lifecycle_event(
    task_id: &str,
    previous: &ClickupTaskSnapshot,
    revision: u64,
) -> ClickupLifecycleEventRecord {
    let key = ClickupLifecycleEventKey::removed(task_id)
        .with_revision(revision)
        .as_task_key();
    ClickupLifecycleEventRecord {
        record_kind: CLICKUP_LIFECYCLE_EVENT_KIND.to_string(),
        key,
        event: "removed".to_string(),
        task_id: task_id.to_string(),
        list_id: previous.list_id.clone(),
        revision,
        snapshot: clickup_previous_snapshot_value(
            &previous.list_id,
            &previous.name,
            &previous.status,
            previous.url.as_deref(),
        ),
        previous_snapshot: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_status_detection_is_case_insensitive() {
        assert!(ClickupStatus::from_optional(Some("Cancelled")).is_terminal());
        assert!(ClickupStatus::from_optional(Some("CLOSED")).is_terminal());
        assert!(ClickupStatus::from_optional(Some("Done")).is_terminal());
        assert!(!ClickupStatus::from_optional(Some("in progress")).is_terminal());
    }

    #[test]
    fn lifecycle_key_format_is_stable() {
        let terminal_status = ClickupStatus::from_optional(Some("Needs Follow Up"));
        assert_eq!(
            ClickupLifecycleEventKey::created("task-1").as_task_key(),
            "clickup-created:task-1"
        );
        assert_eq!(
            ClickupLifecycleEventKey::terminal("task-1", &terminal_status).as_task_key(),
            "clickup-terminal:task-1:needs-follow-up"
        );
        assert_eq!(
            ClickupLifecycleEventKey::removed("task-1").as_task_key(),
            "clickup-removed:task-1"
        );
    }

    #[test]
    fn config_normalization_deduplicates_and_validates() {
        let config = ClickupSourceConfig {
            list_ids: vec!["  L2 ".to_string(), "L1".to_string(), "L2".to_string()],
        };
        assert_eq!(
            config.normalized_list_ids().expect("valid list ids"),
            vec!["L1", "L2"]
        );

        let invalid = ClickupSourceConfig { list_ids: vec![] };
        assert!(invalid.normalized_list_ids().is_err());
    }

    #[test]
    fn source_key_uses_normalized_list_ids() {
        assert_eq!(
            ClickupSourceConfig::source_key(&["  L2 ".into(), "L1".into(), "L2".into()]),
            "clickup:L1,L2"
        );
    }
}
