use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::{
    extract::TaskExtractor,
    model::{
        ProjectContext, ProjectInterpretation, SlackMessage, TaskBatch, TaskSourceKind, unix_now,
    },
    sink::TaskSink,
    state::StateStore,
};

const ERROR_ESCALATION_THRESHOLD: u32 = 3;

#[derive(Debug, Clone)]
pub struct SourcePoll {
    pub source_key: String,
    pub source: TaskSourceKind,
    pub source_label: String,
    pub messages: Vec<SlackMessage>,
}

#[async_trait]
pub trait TaskSource: Send {
    fn source_key(&self) -> String;
    async fn poll(&mut self, state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll>;
}

pub struct TaskDaemon {
    source: Box<dyn TaskSource>,
    extractor: TaskExtractor,
    sinks: Vec<Box<dyn TaskSink>>,
    state_store: StateStore,
    project_context: ProjectContext,
    emit_empty_batches: bool,
}

impl TaskDaemon {
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

    pub fn set_emit_empty_batches(&mut self, emit_empty_batches: bool) {
        self.emit_empty_batches = emit_empty_batches;
    }

    /// Runs one poll/extract/deliver cycle.
    ///
    /// Delivery is intentionally at-most-once: source cursor state is persisted
    /// immediately after polling and before sink delivery. If sink delivery
    /// fails, those source messages are not re-polled on the next run.
    pub async fn run_once(&mut self) -> Result<TaskBatch> {
        let mut state = self.state_store.load().context("loading daemon state")?;
        let poll = self
            .source
            .poll(&mut state)
            .await
            .context("polling source")?;
        self.state_store
            .save(&state)
            .context("saving daemon cursor state")?;

        let mut batch = match poll.source {
            TaskSourceKind::Slack => self
                .extractor
                .extract_slack_runtime(&poll.source_label, &self.project_context, &poll.messages)
                .await
                .context("extracting Slack project interpretation")?,
            TaskSourceKind::GithubIssues => {
                tracing::warn!(
                    source = %poll.source_label,
                    "GitHub issues interpretation is not implemented yet; emitting empty batch"
                );
                TaskBatch {
                    source: TaskSourceKind::GithubIssues,
                    source_label: poll.source_label,
                    generated_at_unix: unix_now(),
                    messages_scanned: poll.messages.len(),
                    project: self.project_context.clone(),
                    interpretation: ProjectInterpretation::default(),
                    derived_tasks: Vec::new(),
                }
            }
        };

        {
            let source_state = state.source_state_mut(&poll.source_key);
            batch
                .derived_tasks
                .retain(|task| !source_state.has_seen_task(&task.key));
        }

        if !batch.derived_tasks.is_empty() || self.emit_empty_batches {
            for sink in &mut self.sinks {
                let sink_name = sink.name();
                sink.deliver(&batch)
                    .await
                    .with_context(|| format!("delivering batch to sink {sink_name}"))?;
            }
        }

        let seen_at = unix_now();
        {
            let source_state = state.source_state_mut(&poll.source_key);
            for task in &batch.derived_tasks {
                source_state.mark_task_seen(task.key.clone(), seen_at);
            }
            source_state.prune_seen_tasks(self.state_store.max_seen_tasks_per_source);
        }

        self.state_store
            .save(&state)
            .context("saving daemon task state")?;
        Ok(batch)
    }

    pub async fn run_loop(&mut self, poll_interval: Duration) -> Result<()> {
        let mut consecutive_failures = 0_u32;

        loop {
            match self.run_once().await {
                Ok(batch) => {
                    if consecutive_failures > 0 {
                        tracing::info!(
                            consecutive_failures,
                            "task-daemon poll recovered after consecutive failures"
                        );
                    }
                    consecutive_failures = 0;
                    tracing::info!(
                        source = %batch.source_label,
                        messages_scanned = batch.messages_scanned,
                        derived_tasks = batch.derived_tasks.len(),
                        "task-daemon poll completed"
                    );
                }
                Err(err) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    if consecutive_failures >= ERROR_ESCALATION_THRESHOLD {
                        tracing::error!(
                            error = %err,
                            consecutive_failures,
                            "task-daemon poll failed repeatedly"
                        );
                    } else {
                        tracing::warn!(
                            error = %err,
                            consecutive_failures,
                            "task-daemon poll failed"
                        );
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::*;
    use crate::{extract::ExtractionMode, model::SourceReference};

    const SOURCE_KEY: &str = "slack:test";
    const CURSOR_TS: &str = "1735689700.000000";

    #[derive(Default)]
    struct CursorOnlySource;

    #[async_trait]
    impl TaskSource for CursorOnlySource {
        fn source_key(&self) -> String {
            SOURCE_KEY.to_string()
        }

        async fn poll(&mut self, state: &mut crate::state::TaskDaemonState) -> Result<SourcePoll> {
            state.source_state_mut(SOURCE_KEY).last_seen_ts = Some(CURSOR_TS.to_string());
            Ok(SourcePoll {
                source_key: SOURCE_KEY.to_string(),
                source: TaskSourceKind::Slack,
                source_label: "#test".to_string(),
                messages: vec![SlackMessage {
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
            })
        }
    }

    struct FailingSink;

    #[async_trait]
    impl TaskSink for FailingSink {
        fn name(&self) -> &'static str {
            "failing-sink"
        }

        async fn deliver(&mut self, _batch: &TaskBatch) -> Result<()> {
            Err(anyhow!("intentional sink failure"))
        }
    }

    #[tokio::test]
    async fn persists_cursor_when_sink_delivery_fails() {
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
        let source_state = persisted_state
            .source_state(SOURCE_KEY)
            .expect("state should include source entry");
        assert_eq!(source_state.last_seen_ts.as_deref(), Some(CURSOR_TS));
    }
}
