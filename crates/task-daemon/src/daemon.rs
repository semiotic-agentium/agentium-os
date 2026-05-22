//! Runs the poll and publish loop for task-daemon.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use baml_rt_core::ProducedEvent;
use baml_rt_observability::metrics;
use baml_tools_clickup::ClickupLifecycleEventRecord;
use thiserror::Error;

use crate::{
    model::{ProjectContext, SlackMessage, TaskSourceKind},
    poll_lineage::mint_poll_lineage,
    sink::TaskSink,
    source_records::poll_to_produced_event,
    state::StateStore,
};

/// Work collected from one source during one polling cycle.
#[derive(Debug, Clone)]
pub struct SourcePoll {
    pub source_key: String,
    pub source_label: String,
    pub source_items_scanned: usize,
    source_cursor: Option<String>,
    payload: SourcePollPayload,
}

#[derive(Debug, Clone)]
enum SourcePollPayload {
    Slack {
        messages: Vec<SlackMessage>,
    },
    Clickup {
        lifecycle_events: Vec<ClickupLifecycleEventRecord>,
    },
}

impl SourcePoll {
    pub fn slack(
        source_key: String,
        source_label: String,
        messages: Vec<SlackMessage>,
        source_items_scanned: usize,
    ) -> Self {
        Self {
            source_key,
            source_label,
            source_items_scanned,
            source_cursor: slack_source_cursor(&messages),
            payload: SourcePollPayload::Slack { messages },
        }
    }

    pub fn clickup(
        source_key: String,
        source_label: String,
        lifecycle_events: Vec<ClickupLifecycleEventRecord>,
        source_items_scanned: usize,
    ) -> Self {
        Self {
            source_key,
            source_label,
            source_items_scanned,
            source_cursor: clickup_source_cursor(&lifecycle_events),
            payload: SourcePollPayload::Clickup { lifecycle_events },
        }
    }

    pub fn source_kind(&self) -> TaskSourceKind {
        self.payload.source_kind()
    }

    pub fn messages(&self) -> &[SlackMessage] {
        match &self.payload {
            SourcePollPayload::Slack { messages } => messages,
            SourcePollPayload::Clickup { .. } => &[],
        }
    }

    pub fn clickup_lifecycle_events(&self) -> &[ClickupLifecycleEventRecord] {
        match &self.payload {
            SourcePollPayload::Slack { .. } => &[],
            SourcePollPayload::Clickup { lifecycle_events } => lifecycle_events,
        }
    }

    pub(crate) fn source_cursor(&self) -> Option<&str> {
        self.source_cursor.as_deref()
    }

    pub fn is_empty(&self) -> bool {
        match &self.payload {
            SourcePollPayload::Slack { messages } => messages.is_empty(),
            SourcePollPayload::Clickup { lifecycle_events } => lifecycle_events.is_empty(),
        }
    }

    fn into_parts(self) -> (String, String, usize, SourcePollPayload) {
        (
            self.source_key,
            self.source_label,
            self.source_items_scanned,
            self.payload,
        )
    }
}

impl SourcePollPayload {
    fn source_kind(&self) -> TaskSourceKind {
        match self {
            Self::Slack { .. } => TaskSourceKind::Slack,
            Self::Clickup { .. } => TaskSourceKind::Clickup,
        }
    }
}

fn slack_source_cursor(messages: &[SlackMessage]) -> Option<String> {
    let first = messages.first()?;
    let last = messages.last()?;
    Some(format!("slack:{}:{}:{}", first.ts, last.ts, messages.len()))
}

fn clickup_source_cursor(events: &[ClickupLifecycleEventRecord]) -> Option<String> {
    if events.is_empty() {
        return None;
    }
    const CURSOR_KEY_SEPARATOR: &str = "\u{1f}";
    let mut task_keys = events
        .iter()
        .map(|task| task.key.as_str())
        .collect::<Vec<_>>();
    task_keys.sort_unstable();
    task_keys.dedup();
    Some(format!("clickup:{}", task_keys.join(CURSOR_KEY_SEPARATOR)))
}

#[async_trait]
pub trait TaskSource: Send {
    fn source_key(&self) -> String;
    fn next_poll_source_key(&self) -> String {
        self.source_key()
    }
    fn polls_per_cycle(&self) -> usize {
        1
    }
    async fn poll(&mut self, state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll>;
}

pub struct RoundRobinTaskSource {
    sources: Vec<Box<dyn TaskSource>>,
    next_index: usize,
}

#[derive(Debug, Error)]
pub enum RoundRobinTaskSourceError {
    #[error("round-robin source requires at least one source")]
    EmptySources,
}

impl RoundRobinTaskSource {
    pub fn new(
        sources: Vec<Box<dyn TaskSource>>,
    ) -> std::result::Result<Self, RoundRobinTaskSourceError> {
        if sources.is_empty() {
            return Err(RoundRobinTaskSourceError::EmptySources);
        }
        Ok(Self {
            sources,
            next_index: 0,
        })
    }
}

#[async_trait]
impl TaskSource for RoundRobinTaskSource {
    fn source_key(&self) -> String {
        "multi-source-round-robin".to_string()
    }

    fn next_poll_source_key(&self) -> String {
        let index = self.next_index % self.sources.len();
        self.sources[index].source_key()
    }

    fn polls_per_cycle(&self) -> usize {
        self.sources.len().max(1)
    }

    async fn poll(&mut self, state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll> {
        let index = self.next_index % self.sources.len();
        self.next_index = (index + 1) % self.sources.len();
        self.sources[index].poll(state).await
    }
}

/// Coordinates source polling, source-record publish, sink delivery, and state persistence.
pub struct TaskDaemon {
    source: Box<dyn TaskSource>,
    sinks: Vec<Box<dyn TaskSink>>,
    state_store: StateStore,
    project_context: ProjectContext,
    emit_empty_batches: bool,
}

impl TaskDaemon {
    pub fn new(
        source: Box<dyn TaskSource>,
        sinks: Vec<Box<dyn TaskSink>>,
        state_store: StateStore,
        project_context: ProjectContext,
    ) -> Self {
        Self {
            source,
            sinks,
            state_store,
            project_context,
            emit_empty_batches: false,
        }
    }

    pub fn set_emit_empty_batches(&mut self, emit_empty_batches: bool) {
        self.emit_empty_batches = emit_empty_batches;
    }

    pub fn polls_per_cycle(&self) -> usize {
        self.source.polls_per_cycle().max(1)
    }

    pub async fn run_once(&mut self) -> Result<ProducedEvent> {
        let cycle_start = std::time::Instant::now();
        let result = self.run_once_impl().await;
        match &result {
            Ok(event) => metrics::record_task_daemon_run_once(
                event.source_kind.as_str(),
                "success",
                cycle_start.elapsed(),
            ),
            Err(_) => {
                metrics::record_task_daemon_run_once("unknown", "error", cycle_start.elapsed())
            }
        }
        result
    }

    async fn run_once_impl(&mut self) -> Result<ProducedEvent> {
        let mut state = self
            .state_store
            .load()
            .await
            .context("loading daemon state")?;
        let poll = self
            .source
            .poll(&mut state)
            .await
            .context("polling source")?;

        let lineage = mint_poll_lineage(&poll)
            .ok_or_else(|| anyhow::anyhow!("poll missing source_cursor; cannot mint lineage"))?;

        let (source_key, source_label, source_items_scanned, payload) = poll.into_parts();
        let source_kind = payload.source_kind();

        let poll_for_event = match payload {
            SourcePollPayload::Slack { messages } => {
                if messages.is_empty() && !self.emit_empty_batches {
                    self.state_store
                        .save(&state)
                        .await
                        .context("saving daemon state")?;
                    return poll_to_produced_event(
                        &SourcePoll::slack(
                            source_key.clone(),
                            source_label.clone(),
                            messages,
                            source_items_scanned,
                        ),
                        &self.project_context,
                        &lineage,
                    );
                }
                SourcePoll::slack(
                    source_key.clone(),
                    source_label,
                    messages,
                    source_items_scanned,
                )
            }
            SourcePollPayload::Clickup {
                mut lifecycle_events,
            } => {
                {
                    let source_state = state.source_state_mut(&source_key);
                    lifecycle_events.retain(|event| !source_state.has_seen_task(&event.key));
                }
                if lifecycle_events.is_empty() && !self.emit_empty_batches {
                    self.state_store
                        .save(&state)
                        .await
                        .context("saving daemon state")?;
                    return poll_to_produced_event(
                        &SourcePoll::clickup(
                            source_key.clone(),
                            source_label.clone(),
                            lifecycle_events,
                            source_items_scanned,
                        ),
                        &self.project_context,
                        &lineage,
                    );
                }
                SourcePoll::clickup(
                    source_key.clone(),
                    source_label,
                    lifecycle_events,
                    source_items_scanned,
                )
            }
        };

        let event = poll_to_produced_event(&poll_for_event, &self.project_context, &lineage)
            .context("building produced event")?;

        let should_deliver = !poll_for_event.is_empty() || self.emit_empty_batches;
        if should_deliver {
            let mut delivered = false;
            for sink in &mut self.sinks {
                if !sink.accepts_source(source_kind) {
                    continue;
                }
                delivered = true;
                sink.deliver(&event)
                    .await
                    .with_context(|| format!("delivering to sink {}", sink.name()))?;
            }
            if !delivered && !poll_for_event.is_empty() {
                bail!(
                    "no compatible sinks configured for source {:?}",
                    source_kind
                );
            }
            if matches!(poll_for_event.source_kind(), TaskSourceKind::Clickup) {
                let source_state = state.source_state_mut(&source_key);
                let seen_at =
                    baml_rt_core::now_unix_secs(baml_rt_core::clock_events::TASK_DAEMON_SEEN_TASK);
                for event in poll_for_event.clickup_lifecycle_events() {
                    source_state.mark_task_seen(event.key.clone(), seen_at);
                }
            }
        }

        self.state_store
            .save(&state)
            .await
            .context("saving daemon state")?;
        Ok(event)
    }

    pub async fn run_loop(&mut self, interval: Duration) -> Result<()> {
        loop {
            if let Err(err) = self.run_once().await {
                tracing::error!(error = %err, "task-daemon cycle failed");
            }
            tokio::time::sleep(interval).await;
        }
    }
}
