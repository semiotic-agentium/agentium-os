//! ClickUp polling implementation for [`crate::daemon::TaskSource`].

use std::{collections::BTreeMap, fmt};

use anyhow::{Context, Result};
use async_trait::async_trait;
use integrations_clickup_client::ClickUpClient;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    daemon::{SourcePoll, TaskSource},
    model::{InvestigationTask, SourceReference, TaskConfidence},
    state::{ClickupTaskSnapshot, TaskDaemonState},
};

const MAX_CLICKUP_LIST_TASK_PAGES: u32 = 50;

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
    revisions: &mut BTreeMap<String, u64>,
    kind: ClickupLifecycleEventKind,
    task_id: &str,
) -> u64 {
    let slot = format!("{}:{task_id}", kind.revision_counter_prefix());
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

impl From<ClickupPriority> for TaskConfidence {
    fn from(value: ClickupPriority) -> Self {
        match value {
            ClickupPriority::Urgent | ClickupPriority::High => TaskConfidence::High,
            ClickupPriority::Low => TaskConfidence::Low,
            ClickupPriority::Normal | ClickupPriority::Unknown => TaskConfidence::Medium,
        }
    }
}

#[derive(Debug, Error)]
/// Typed source-construction failures for ClickUp polling.
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
    fn normalized_list_ids(
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
}

#[derive(Clone)]
/// ClickUp-backed task source that emits created/terminal/removed task events.
pub struct ClickupTaskSource {
    client: ClickUpClient,
    list_ids: Vec<ClickupListId>,
}

impl ClickupTaskSource {
    /// Creates a ClickUp source with the given configuration.
    pub fn new(config: ClickupSourceConfig) -> std::result::Result<Self, ClickupSourceConfigError> {
        // Validate upfront so source errors are operational, not configuration mistakes.
        let list_ids = config.normalized_list_ids()?;
        Ok(Self {
            client: ClickUpClient::new(),
            list_ids,
        })
    }

    fn source_key(list_ids: &[ClickupListId]) -> String {
        format!(
            "clickup:{}",
            list_ids
                .iter()
                .map(ClickupListId::as_str)
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    fn source_label(list_ids: &[ClickupListId]) -> String {
        if list_ids.len() == 1 {
            return format!("clickup:list:{}", list_ids[0]);
        }
        format!(
            "clickup:lists:{}",
            list_ids
                .iter()
                .map(ClickupListId::as_str)
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    async fn fetch_list_tasks(
        &self,
        api_key: &str,
        list_id: &ClickupListId,
    ) -> Result<Vec<ClickupTaskRecord>> {
        let mut out = Vec::new();
        let mut page = 0_u32;
        let mut previous_page_signature: Option<Vec<String>> = None;

        loop {
            let page_value = page.to_string();
            let json = self
                .client
                .send_json(
                    self.client
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

            // Continue paging while ClickUp returns raw tasks, even if all entries on a page fail
            // per-task parsing, so one malformed page does not hide later valid pages.
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
}

fn raw_page_signature(tasks: &[Value]) -> Vec<String> {
    tasks.iter().map(Value::to_string).collect()
}

#[async_trait]
impl TaskSource for ClickupTaskSource {
    fn source_key(&self) -> String {
        Self::source_key(&self.list_ids)
    }

    async fn poll(&mut self, state: &mut TaskDaemonState) -> Result<SourcePoll> {
        let list_ids = &self.list_ids;
        let source_key = Self::source_key(list_ids);
        let source_label = Self::source_label(list_ids);

        // Resolve per poll so rotated credentials can take effect without restart.
        let api_key = ClickUpClient::api_key().context("loading CLICKUP_API_KEY for source")?;

        let previous_source_state = state.source_state(&source_key).cloned().unwrap_or_default();
        let previous_snapshot = previous_source_state.clickup_task_snapshot;
        let mut lifecycle_revisions = previous_source_state.clickup_lifecycle_revisions;

        let mut current_snapshot: BTreeMap<String, ClickupTaskSnapshot> = BTreeMap::new();
        let mut current_records: BTreeMap<String, ClickupTaskRecord> = BTreeMap::new();

        for list_id in list_ids {
            for task in self.fetch_list_tasks(&api_key, list_id).await? {
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

        let mut inferred_tasks = Vec::new();

        for (task_id, task) in &current_records {
            if !previous_snapshot.contains_key(task_id) {
                let revision = next_lifecycle_revision(
                    &mut lifecycle_revisions,
                    ClickupLifecycleEventKind::Created,
                    task_id,
                );
                inferred_tasks.push(created_investigation_task(task, revision));
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
                    inferred_tasks
                        .push(terminal_status_investigation_task(task, previous, revision));
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
                inferred_tasks.push(removed_investigation_task(task_id, previous, revision));
            }
        }

        let source_state = state.source_state_mut(&source_key);
        source_state.clickup_task_snapshot = current_snapshot;
        source_state.clickup_lifecycle_revisions = lifecycle_revisions;

        Ok(SourcePoll::clickup(
            source_key,
            source_label,
            inferred_tasks,
            current_records.len(),
        ))
    }
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

fn created_investigation_task(task: &ClickupTaskRecord, revision: u64) -> InvestigationTask {
    let mut description = format!(
        "New ClickUp task was created in monitored list {list_id}.\nTask ID: {task_id}\nStatus: {status}",
        list_id = task.list_id,
        task_id = task.id,
        status = task.status.original,
    );
    if let Some(url) = &task.url {
        description.push_str(&format!("\nURL: {url}"));
    }
    if let Some(original) = &task.description {
        description.push_str(&format!("\n\nOriginal task description:\n{original}"));
    }

    InvestigationTask {
        key: ClickupLifecycleEventKey::created(&task.id)
            .with_revision(revision)
            .as_task_key(),
        title: format!("Execute ClickUp task: {}", task.name),
        description,
        priority: task.priority.into(),
        sources: vec![clickup_source_reference(&task.id, task.url.as_deref())],
    }
}

fn terminal_status_investigation_task(
    task: &ClickupTaskRecord,
    previous: &ClickupTaskSnapshot,
    revision: u64,
) -> InvestigationTask {
    let mut description = format!(
        "ClickUp task entered a terminal status while monitored.\nTask ID: {task_id}\nPrevious status: {previous_status}\nCurrent status: {current_status}\nList: {list_id}\nStop or reconcile in-flight agent work for this task.",
        task_id = task.id,
        previous_status = previous.status,
        current_status = task.status.original,
        list_id = task.list_id,
    );
    if let Some(url) = &task.url {
        description.push_str(&format!("\nURL: {url}"));
    }

    InvestigationTask {
        key: ClickupLifecycleEventKey::terminal(&task.id, &task.status)
            .with_revision(revision)
            .as_task_key(),
        title: format!("Reconcile terminal ClickUp task: {}", task.name),
        description,
        priority: TaskConfidence::High,
        sources: vec![clickup_source_reference(&task.id, task.url.as_deref())],
    }
}

fn removed_investigation_task(
    task_id: &str,
    previous: &ClickupTaskSnapshot,
    revision: u64,
) -> InvestigationTask {
    let mut description = format!(
        "Previously tracked ClickUp task is no longer present in monitored list output.\nTask ID: {task_id}\nLast known list: {list_id}\nLast known status: {status}\nThis may indicate deletion, archival, or list migration; reconcile active execution.",
        task_id = task_id,
        list_id = previous.list_id,
        status = previous.status,
    );
    if let Some(url) = previous.url.as_deref() {
        description.push_str(&format!("\nLast known URL: {url}"));
    }

    InvestigationTask {
        key: ClickupLifecycleEventKey::removed(task_id)
            .with_revision(revision)
            .as_task_key(),
        title: format!("Reconcile missing ClickUp task: {task_id}"),
        description,
        priority: TaskConfidence::High,
        sources: vec![clickup_source_reference(task_id, previous.url.as_deref())],
    }
}

fn clickup_source_reference(task_id: &str, url: Option<&str>) -> SourceReference {
    SourceReference {
        reference: format!("clickup://task/{task_id}"),
        permalink: url.map(ToString::to_string),
        channel_id: None,
        message_ts: None,
        thread_ts: None,
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
    fn lifecycle_key_uses_revision_suffix_after_first_event() {
        assert_eq!(
            ClickupLifecycleEventKey::created("task-1")
                .with_revision(2)
                .as_task_key(),
            "clickup-created:task-1:r2"
        );
    }

    #[test]
    fn config_normalization_deduplicates_and_validates() {
        let config = ClickupSourceConfig {
            list_ids: vec!["  L2 ".to_string(), "L1".to_string(), "L2".to_string()],
        };
        assert_eq!(
            config
                .normalized_list_ids()
                .expect("valid list ids")
                .iter()
                .map(ClickupListId::as_str)
                .collect::<Vec<_>>(),
            vec!["L1", "L2"]
        );

        let invalid = ClickupSourceConfig { list_ids: vec![] };
        assert!(invalid.normalized_list_ids().is_err());
    }

    #[test]
    fn source_key_uses_cached_normalized_list_ids() {
        let source = ClickupTaskSource::new(ClickupSourceConfig {
            list_ids: vec!["  L2 ".to_string(), "L1".to_string(), "L2".to_string()],
        })
        .expect("valid source config");

        assert_eq!(source.source_key(), "clickup:L1,L2");
    }

    #[test]
    fn created_description_uses_real_newlines() {
        let task = ClickupTaskRecord {
            id: "task-123".to_string(),
            list_id: ClickupListId::parse("L1").expect("valid list id"),
            name: "Investigate".to_string(),
            status: ClickupStatus::from_optional(Some("Open")),
            description: Some("Original description".to_string()),
            url: Some("https://app.clickup.com/t/task-123".to_string()),
            priority: ClickupPriority::High,
        };

        let description = created_investigation_task(&task, 1).description;
        assert!(description.contains('\n'));
        assert!(!description.contains("\\n"));
    }

    #[test]
    fn terminal_and_removed_descriptions_use_real_newlines() {
        let task = ClickupTaskRecord {
            id: "task-123".to_string(),
            list_id: ClickupListId::parse("L1").expect("valid list id"),
            name: "Investigate".to_string(),
            status: ClickupStatus::from_optional(Some("Closed")),
            description: None,
            url: Some("https://app.clickup.com/t/task-123".to_string()),
            priority: ClickupPriority::High,
        };
        let previous = ClickupTaskSnapshot {
            list_id: "L1".to_string(),
            name: "Investigate".to_string(),
            status: "In Progress".to_string(),
            url: Some("https://app.clickup.com/t/task-123".to_string()),
        };

        let terminal_description =
            terminal_status_investigation_task(&task, &previous, 1).description;
        assert!(terminal_description.contains('\n'));
        assert!(!terminal_description.contains("\\n"));

        let removed_description = removed_investigation_task(&task.id, &previous, 1).description;
        assert!(removed_description.contains('\n'));
        assert!(!removed_description.contains("\\n"));
    }
}
