//! Runs the poll, interpret, and deliver loop for task-daemon.

use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use thiserror::Error;

use crate::{
    contract::{InterpretationRequestEvent, TaskDispatch},
    extract::TaskExtractor,
    model::{
        InvestigationTask, ProjectContext, ProjectInterpretation, SlackMessage, TaskBatch,
        TaskSourceKind, unix_now,
    },
    sink::TaskSink,
    state::StateStore,
};

const ERROR_ESCALATION_THRESHOLD: u32 = 3;

#[derive(Debug, Clone)]
/// Work collected from one source during one polling cycle.
pub struct SourcePoll {
    /// Stable key used to resume this source safely.
    pub source_key: String,
    /// Human-readable source label (for example `#agentium-eng`).
    pub source_label: String,
    /// Number of source items scanned during this poll cycle.
    pub source_items_scanned: usize,
    payload: SourcePollPayload,
}

#[derive(Debug, Clone)]
enum SourcePollPayload {
    // Slack messages that still need interpretation.
    Slack {
        messages: Vec<SlackMessage>,
    },
    // ClickUp lifecycle events already represented as investigation tasks.
    Clickup {
        inferred_tasks: Vec<InvestigationTask>,
    },
}

impl SourcePoll {
    /// Creates a source poll from Slack messages.
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
            payload: SourcePollPayload::Slack { messages },
        }
    }

    /// Creates a source poll from ClickUp-derived tasks.
    pub fn clickup(
        source_key: String,
        source_label: String,
        inferred_tasks: Vec<InvestigationTask>,
        source_items_scanned: usize,
    ) -> Self {
        Self {
            source_key,
            source_label,
            source_items_scanned,
            payload: SourcePollPayload::Clickup { inferred_tasks },
        }
    }

    /// Returns the source kind for this poll result.
    pub fn source_kind(&self) -> TaskSourceKind {
        self.payload.source_kind()
    }

    /// Returns Slack messages when the source is Slack; otherwise empty.
    pub fn messages(&self) -> &[SlackMessage] {
        match &self.payload {
            SourcePollPayload::Slack { messages } => messages,
            SourcePollPayload::Clickup { .. } => &[],
        }
    }

    /// Returns pre-derived tasks for sources that already provide them.
    pub fn inferred_tasks(&self) -> &[InvestigationTask] {
        match &self.payload {
            SourcePollPayload::Slack { .. } => &[],
            SourcePollPayload::Clickup { inferred_tasks } => inferred_tasks,
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

#[async_trait]
/// A work source that task-daemon can poll.
pub trait TaskSource: Send {
    /// Stable state key for this source instance.
    fn source_key(&self) -> String;
    /// Source key that will be polled on the next [`Self::poll`] call.
    fn next_poll_source_key(&self) -> String {
        self.source_key()
    }
    /// Number of polls required to cover one full source cycle.
    fn polls_per_cycle(&self) -> usize {
        1
    }
    /// Poll new data using (and mutating) persisted daemon state.
    async fn poll(&mut self, state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll>;
}

/// Round-robin multiplexer for polling multiple sources with one daemon loop.
pub struct RoundRobinTaskSource {
    sources: Vec<Box<dyn TaskSource>>,
    next_index: usize,
}

#[derive(Debug, Error)]
/// Typed round-robin source-construction failures.
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

/// Coordinates source polling, interpretation, sink delivery, and state persistence.
pub struct TaskDaemon {
    source: Box<dyn TaskSource>,
    extractor: TaskExtractor,
    sinks: Vec<Box<dyn TaskSink>>,
    state_store: StateStore,
    project_context: ProjectContext,
    emit_empty_batches: bool,
}

impl TaskDaemon {
    /// Builds a daemon from one source, one extractor, and one or more sinks.
    pub fn new(
        source: Box<dyn TaskSource>,
        extractor: TaskExtractor,
        sinks: Vec<Box<dyn TaskSink>>,
        state_store: StateStore,
        project_context: ProjectContext,
    ) -> Self {
        Self {
            source,
            extractor,
            sinks,
            state_store,
            project_context,
            emit_empty_batches: false,
        }
    }

    /// When enabled, batches with no derived tasks are still delivered to sinks.
    pub fn set_emit_empty_batches(&mut self, emit_empty_batches: bool) {
        self.emit_empty_batches = emit_empty_batches;
    }

    /// Returns how many source polls should be executed to cover one source cycle.
    pub fn polls_per_cycle(&self) -> usize {
        self.source.polls_per_cycle().max(1)
    }

    /// Runs one poll/extract/deliver cycle.
    ///
    /// Delivery is best-effort at-least-once for successfully interpreted polls:
    /// source cursor/task state is persisted only after sink delivery succeeds.
    /// If sink delivery fails, source state is not committed so the poll window can be retried.
    pub async fn run_once(&mut self) -> Result<TaskDispatch> {
        let mut state = self.state_store.load().context("loading daemon state")?;
        let poll = self
            .source
            .poll(&mut state)
            .await
            .context("polling source")?;
        let request_event =
            InterpretationRequestEvent::from_source_poll(&poll, self.project_context.clone(), None);
        let (source_key, source_label, source_items_scanned, payload) = poll.into_parts();
        let source_kind = payload.source_kind();

        let mut batch = match payload {
            SourcePollPayload::Slack { messages } => self
                .extractor
                .extract_slack_runtime(&source_label, &self.project_context, &messages)
                .await
                .context("extracting Slack project interpretation")?,
            SourcePollPayload::Clickup { inferred_tasks } => TaskBatch {
                source: TaskSourceKind::Clickup,
                source_label,
                generated_at_unix: unix_now(),
                messages_scanned: source_items_scanned,
                project: self.project_context.clone(),
                interpretation: clickup_interpretation_for_batch(
                    source_items_scanned,
                    inferred_tasks.len(),
                ),
                derived_tasks: inferred_tasks,
            },
        };
        if matches!(source_kind, TaskSourceKind::Slack) {
            batch.messages_scanned = source_items_scanned;
        }

        {
            let source_state = state.source_state_mut(&source_key);
            batch
                .derived_tasks
                .retain(|task| !source_state.has_seen_task(&task.key));
        }

        let dispatch = TaskDispatch::from_batch(request_event, batch);

        if !dispatch.batch.derived_tasks.is_empty() || self.emit_empty_batches {
            let mut delivered_to_compatible_sink = false;
            for sink in &mut self.sinks {
                if !sink.accepts_source(dispatch.batch.source) {
                    tracing::debug!(
                        source = ?dispatch.batch.source,
                        source_label = %dispatch.batch.source_label,
                        sink = sink.name(),
                        "skipping incompatible sink for source batch"
                    );
                    continue;
                }
                delivered_to_compatible_sink = true;
                let sink_name = sink.name();
                sink.deliver(&dispatch)
                    .await
                    .with_context(|| format!("delivering batch to sink {sink_name}"))?;
            }
            if !delivered_to_compatible_sink && !dispatch.batch.derived_tasks.is_empty() {
                bail!(
                    "no compatible sinks configured for source {:?}; cannot deliver {} derived task(s)",
                    dispatch.batch.source,
                    dispatch.batch.derived_tasks.len()
                );
            }
        }

        let seen_at = unix_now();
        {
            let source_state = state.source_state_mut(&source_key);
            for task in &dispatch.batch.derived_tasks {
                source_state.mark_task_seen(task.key.clone(), seen_at);
            }
            source_state.prune_seen_tasks(self.state_store.max_seen_tasks_per_source);
        }

        self.state_store
            .save(&state)
            .context("saving daemon state")?;
        Ok(dispatch)
    }

    /// Runs [`Self::run_once`] forever with a fixed delay between iterations.
    pub async fn run_loop(&mut self, poll_interval: Duration) -> Result<()> {
        let mut consecutive_failures_by_source: BTreeMap<String, u32> = BTreeMap::new();
        let polls_per_interval = self.polls_per_cycle();

        loop {
            for _ in 0..polls_per_interval {
                let source_key = self.source.next_poll_source_key();
                match self.run_once().await {
                    Ok(dispatch) => {
                        let consecutive_failures = consecutive_failures_by_source
                            .remove(&source_key)
                            .unwrap_or(0);
                        if consecutive_failures > 0 {
                            tracing::info!(
                                consecutive_failures,
                                source_key = %source_key,
                                "task-daemon poll recovered after consecutive failures"
                            );
                        }
                        tracing::info!(
                            source_key = %source_key,
                            source = %dispatch.batch.source_label,
                            messages_scanned = dispatch.batch.messages_scanned,
                            derived_tasks = dispatch.batch.derived_tasks.len(),
                            "task-daemon poll completed"
                        );
                    }
                    Err(err) => {
                        let failures_for_source = consecutive_failures_by_source
                            .entry(source_key.clone())
                            .or_insert(0);
                        let next_failures = failures_for_source.saturating_add(1);
                        *failures_for_source = next_failures;
                        if next_failures >= ERROR_ESCALATION_THRESHOLD {
                            tracing::error!(
                                error = %err,
                                source_key = %source_key,
                                consecutive_failures = next_failures,
                                "task-daemon poll failed repeatedly"
                            );
                        } else {
                            tracing::warn!(
                                error = %err,
                                source_key = %source_key,
                                consecutive_failures = next_failures,
                                "task-daemon poll failed"
                            );
                        }
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }
}

fn clickup_interpretation_for_batch(
    source_items_scanned: usize,
    derived_task_count: usize,
) -> ProjectInterpretation {
    if derived_task_count == 0 {
        return ProjectInterpretation {
            executive_summary: format!(
                "No new ClickUp task lifecycle events detected in {source_items_scanned} scanned task(s)."
            ),
            ..ProjectInterpretation::default()
        };
    }

    ProjectInterpretation {
        executive_summary: format!(
            "Detected {derived_task_count} ClickUp task event(s) from {source_items_scanned} scanned task(s); preparing agent handoff."
        ),
        current_objectives: vec![
            "Acknowledge newly created ClickUp tasks and trigger execution handoff.".to_string(),
        ],
        ..ProjectInterpretation::default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::*;
    use crate::{extract::ExtractionMode, model::SourceReference};

    const SOURCE_KEY: &str = "slack:test";
    const CURSOR_TS: &str = "1735689700.000000";

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: test-only mutation guarded by a process-wide mutex in each test.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: test-only mutation guarded by a process-wide mutex in each test.
            unsafe { std::env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.as_deref() {
                // SAFETY: restoring test-only env state under the same mutex guard.
                unsafe { std::env::set_var(self.key, value) };
            } else {
                // SAFETY: restoring test-only env state under the same mutex guard.
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[derive(Default)]
    struct CursorOnlySource;

    #[async_trait]
    impl TaskSource for CursorOnlySource {
        fn source_key(&self) -> String {
            SOURCE_KEY.to_string()
        }

        async fn poll(&mut self, state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll> {
            state.source_state_mut(SOURCE_KEY).last_seen_ts = Some(CURSOR_TS.to_string());
            Ok(SourcePoll::slack(
                SOURCE_KEY.to_string(),
                "#test".to_string(),
                vec![SlackMessage {
                    channel_name: "test".to_string(),
                    channel_id: "C123".to_string(),
                    ts: CURSOR_TS.to_string(),
                    thread_ts: None,
                    user_id: Some("U123".to_string()),
                    user_name: Some("alice".to_string()),
                    text: "TODO: update docs".to_string(),
                    subtype: None,
                    source: SourceReference {
                        reference: "slack://channel/C123/p1735689700000000".to_string(),
                        permalink: None,
                        channel_id: Some("C123".to_string()),
                        message_ts: Some(CURSOR_TS.to_string()),
                        thread_ts: None,
                    },
                }],
                1,
            ))
        }
    }

    #[derive(Clone)]
    struct FixedKeySource {
        key: &'static str,
    }

    #[async_trait]
    impl TaskSource for FixedKeySource {
        fn source_key(&self) -> String {
            self.key.to_string()
        }

        async fn poll(&mut self, _state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll> {
            Ok(SourcePoll::slack(
                self.source_key(),
                format!("#{}", self.key),
                Vec::new(),
                0,
            ))
        }
    }

    #[derive(Default)]
    struct ClickupStubSource;

    #[async_trait]
    impl TaskSource for ClickupStubSource {
        fn source_key(&self) -> String {
            "clickup:test-list".to_string()
        }

        async fn poll(&mut self, _state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll> {
            Ok(SourcePoll::clickup(
                self.source_key(),
                "clickup:list:test-list".to_string(),
                vec![InvestigationTask {
                    key: "clickup-created:task-1".to_string(),
                    title: "Execute ClickUp task".to_string(),
                    description: "handoff".to_string(),
                    priority: crate::model::TaskConfidence::High,
                    sources: Vec::new(),
                }],
                1,
            ))
        }
    }

    struct FailingSink;

    #[async_trait]
    impl TaskSink for FailingSink {
        fn name(&self) -> &'static str {
            "failing-sink"
        }

        async fn deliver(&mut self, _dispatch: &TaskDispatch) -> Result<()> {
            Err(anyhow!("intentional sink failure"))
        }
    }

    struct SlackCompatibleSink;

    #[async_trait]
    impl TaskSink for SlackCompatibleSink {
        fn name(&self) -> &'static str {
            "slack-only"
        }

        fn accepts_source(&self, source: TaskSourceKind) -> bool {
            matches!(source, TaskSourceKind::Slack)
        }

        async fn deliver(&mut self, _dispatch: &TaskDispatch) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn round_robin_reports_full_cycle_poll_count() {
        let source =
            RoundRobinTaskSource::new(vec![Box::new(CursorOnlySource), Box::new(CursorOnlySource)])
                .expect("round-robin source");
        assert_eq!(source.polls_per_cycle(), 2);
    }

    #[tokio::test]
    async fn round_robin_next_poll_source_key_tracks_rotation() {
        let mut source = RoundRobinTaskSource::new(vec![
            Box::new(FixedKeySource { key: "source-a" }),
            Box::new(FixedKeySource { key: "source-b" }),
        ])
        .expect("round-robin source");
        assert_eq!(source.next_poll_source_key(), "source-a");

        let mut state = crate::state::TaskDaemonState::default();
        source.poll(&mut state).await.expect("first poll");
        assert_eq!(source.next_poll_source_key(), "source-b");
    }

    #[test]
    fn source_poll_slack_payload_has_messages_only() {
        let poll = SourcePoll::slack(
            "slack:test".to_string(),
            "#test".to_string(),
            vec![SlackMessage {
                channel_name: "agentium-eng".to_string(),
                channel_id: "C123".to_string(),
                ts: CURSOR_TS.to_string(),
                thread_ts: None,
                user_id: None,
                user_name: None,
                text: "message".to_string(),
                subtype: None,
                source: SourceReference {
                    reference: "slack://channel/C123/p1735689700000000".to_string(),
                    permalink: None,
                    channel_id: Some("C123".to_string()),
                    message_ts: Some(CURSOR_TS.to_string()),
                    thread_ts: None,
                },
            }],
            1,
        );

        assert_eq!(poll.source_kind(), TaskSourceKind::Slack);
        assert_eq!(poll.messages().len(), 1);
        assert!(poll.inferred_tasks().is_empty());
    }

    #[test]
    fn source_poll_clickup_payload_has_inferred_tasks_only() {
        let poll = SourcePoll::clickup(
            "clickup:L1".to_string(),
            "clickup:list:L1".to_string(),
            vec![InvestigationTask {
                key: "k".to_string(),
                title: "t".to_string(),
                description: "d".to_string(),
                priority: crate::model::TaskConfidence::Medium,
                sources: Vec::new(),
            }],
            1,
        );

        assert_eq!(poll.source_kind(), TaskSourceKind::Clickup);
        assert_eq!(poll.inferred_tasks().len(), 1);
        assert!(poll.messages().is_empty());
    }

    #[tokio::test]
    async fn does_not_persist_cursor_when_sink_delivery_fails() {
        let temp = tempdir().expect("create temp directory");
        let state_path = temp.path().join("task-daemon-state.json");
        let store_for_daemon = StateStore::new(state_path.clone(), 100);

        let mut daemon = TaskDaemon::new(
            Box::new(CursorOnlySource),
            TaskExtractor::with_mode(20, ExtractionMode::Heuristic).expect("extractor"),
            vec![Box::new(FailingSink)],
            store_for_daemon,
            ProjectContext {
                project_key: "test-project".to_string(),
                repo_available: false,
                repo_path: None,
            },
        );

        let result = daemon.run_once().await;
        assert!(result.is_err());

        let persisted_state = StateStore::new(state_path, 100)
            .load()
            .expect("load persisted state");
        assert!(
            persisted_state.source_state(SOURCE_KEY).is_none(),
            "cursor state must not be persisted when sink delivery fails"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: env-var mutex serialises test-only env mutation
    async fn does_not_persist_cursor_when_extraction_fails() {
        let _env = env_lock().lock().expect("lock env mutation");
        let _base = EnvVarGuard::set("TASK_DAEMON_LLM_BASE_URL", "http://127.0.0.1:9");
        let _fallback_base = EnvVarGuard::unset("TASK_DAEMON_LLM_FALLBACK_BASE_URL");
        let _fallback_key = EnvVarGuard::unset("TASK_DAEMON_LLM_FALLBACK_API_KEY");
        let _fallback_model = EnvVarGuard::unset("TASK_DAEMON_LLM_FALLBACK_MODEL");

        let temp = tempdir().expect("create temp directory");
        let state_path = temp.path().join("task-daemon-state.json");
        let store_for_daemon = StateStore::new(state_path.clone(), 100);

        let mut daemon = TaskDaemon::new(
            Box::new(CursorOnlySource),
            TaskExtractor::with_mode(20, ExtractionMode::Llm).expect("llm extractor"),
            Vec::new(),
            store_for_daemon,
            ProjectContext {
                project_key: "test-project".to_string(),
                repo_available: false,
                repo_path: None,
            },
        );

        let result = daemon.run_once().await;
        assert!(result.is_err(), "expected extraction to fail");

        let persisted_state = StateStore::new(state_path, 100)
            .load()
            .expect("load persisted state");
        assert!(
            persisted_state.source_state(SOURCE_KEY).is_none(),
            "cursor state must not be persisted when extraction fails"
        );
    }

    #[tokio::test]
    async fn does_not_persist_state_when_no_compatible_sink_for_source() {
        let temp = tempdir().expect("create temp directory");
        let state_path = temp.path().join("task-daemon-state.json");
        let store_for_daemon = StateStore::new(state_path.clone(), 100);

        let mut daemon = TaskDaemon::new(
            Box::new(ClickupStubSource),
            TaskExtractor::with_mode(20, ExtractionMode::Heuristic).expect("extractor"),
            vec![Box::new(SlackCompatibleSink)],
            store_for_daemon,
            ProjectContext {
                project_key: "test-project".to_string(),
                repo_available: false,
                repo_path: None,
            },
        );

        let err = daemon
            .run_once()
            .await
            .expect_err("expected incompatible sink configuration error");
        assert!(
            err.to_string()
                .contains("no compatible sinks configured for source Clickup"),
            "unexpected error: {err:#}"
        );

        let persisted_state = StateStore::new(state_path, 100)
            .load()
            .expect("load persisted state");
        assert!(
            persisted_state.source_state("clickup:test-list").is_none(),
            "state must not be persisted when no compatible sink exists"
        );
    }
}
