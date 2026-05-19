//! Build [`ProducedEvent`] payloads for host pub/sub (`host.source-records.v1`).

use anyhow::{Context, Result, anyhow};
use baml_rt_core::{
    DispatchMetadata, EventSourceKind, ProducedEvent, clock_events,
    event_subscription::EventSourceKey, host_wire::wire,
};
use baml_tools_clickup::{
    ClickupLifecycleTaskInput, ClickupProjectContext, batch_from_lifecycle_tasks,
};
use baml_tools_github::{
    GithubIssueRecordInput, GithubIssuesProjectContext, batch_from_issue_records,
};
use baml_tools_slack::{normalize::normalize_polling_batch, slack_history_row_value};
use serde_json::Value;

use crate::{
    contract::ContractProvenance,
    daemon::SourcePoll,
    model::{InvestigationTask, ProjectContext, SlackMessage, TaskSourceKind},
};

fn event_source_kind(source: TaskSourceKind) -> Result<EventSourceKind> {
    EventSourceKind::parse(source.as_str()).ok_or_else(|| {
        anyhow!(
            "task source kind '{}' is not registered as an EventSourceKind",
            source.as_str()
        )
    })
}

fn slack_channel_id(source_key: &str) -> Result<&str> {
    source_key
        .strip_prefix("slack:")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("slack source_key must be slack:<channel_id>, got {source_key}"))
}

fn slack_messages_to_values(messages: &[SlackMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            slack_history_row_value(
                &message.ts,
                message.thread_ts.as_deref(),
                message.user_id.as_deref(),
                &message.text,
                message.subtype.as_deref(),
            )
        })
        .collect()
}

fn clickup_project_context(project: &ProjectContext) -> ClickupProjectContext {
    ClickupProjectContext {
        project_key: project.project_key.clone(),
        repo_available: project.repo_available,
        repo_path: project.repo_path.clone(),
    }
}

fn github_project_context(project: &ProjectContext) -> GithubIssuesProjectContext {
    GithubIssuesProjectContext {
        project_key: project.project_key.clone(),
        repo_available: project.repo_available,
        repo_path: project.repo_path.clone(),
    }
}

type InvestigationRecordFields = (String, String, String, String, Vec<Value>);

fn investigation_task_record_inputs(
    tasks: &[InvestigationTask],
) -> Result<Vec<InvestigationRecordFields>> {
    tasks
        .iter()
        .map(|task| {
            let sources = task
                .sources
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
                .context("serializing investigation task source references")?;
            Ok((
                task.key.clone(),
                task.title.clone(),
                task.description.clone(),
                task.priority.to_string(),
                sources,
            ))
        })
        .collect()
}

fn clickup_task_inputs(tasks: &[InvestigationTask]) -> Result<Vec<ClickupLifecycleTaskInput>> {
    Ok(investigation_task_record_inputs(tasks)?
        .into_iter()
        .map(
            |(key, title, description, priority, sources)| ClickupLifecycleTaskInput {
                key,
                title,
                description,
                priority,
                sources,
            },
        )
        .collect())
}

fn github_issue_inputs(tasks: &[InvestigationTask]) -> Result<Vec<GithubIssueRecordInput>> {
    Ok(investigation_task_record_inputs(tasks)?
        .into_iter()
        .map(
            |(key, title, description, priority, sources)| GithubIssueRecordInput {
                key,
                title,
                description,
                priority,
                sources,
            },
        )
        .collect())
}

/// Convert one polled source window into a host-routable [`ProducedEvent`].
pub fn poll_to_produced_event(
    poll: &SourcePoll,
    project: &ProjectContext,
    provenance: Option<ContractProvenance>,
) -> Result<ProducedEvent> {
    let emitted_at_unix = baml_rt_core::now_unix_secs(clock_events::TASK_DAEMON_BATCH);
    let source_key = EventSourceKey::parse(&poll.source_key)
        .with_context(|| format!("invalid source_key {:?}", poll.source_key))?;
    let source_kind = event_source_kind(poll.source_kind())?;

    let payload = match poll.source_kind() {
        TaskSourceKind::Slack => {
            let channel_id = slack_channel_id(&poll.source_key)?;
            let raw = slack_messages_to_values(poll.messages());
            let batch = normalize_polling_batch(
                wire::HOST_SOURCE_RECORDS_V1,
                channel_id,
                &source_key,
                &poll.source_label,
                &raw,
                emitted_at_unix,
            );
            serde_json::to_value(&batch).context("serializing slack source-records batch")?
        }
        TaskSourceKind::Clickup => {
            let tasks = clickup_task_inputs(poll.inferred_tasks())?;
            let batch = batch_from_lifecycle_tasks(
                &poll.source_key,
                &poll.source_label,
                Some(clickup_project_context(project)),
                &tasks,
                emitted_at_unix,
            );
            serde_json::to_value(&batch).context("serializing clickup source-records batch")?
        }
        TaskSourceKind::GithubIssues => {
            let records = github_issue_inputs(poll.inferred_tasks())?;
            let batch = batch_from_issue_records(
                &poll.source_key,
                &poll.source_label,
                Some(github_project_context(project)),
                &records,
                emitted_at_unix,
            );
            serde_json::to_value(&batch)
                .context("serializing github_issues source-records batch")?
        }
    };

    let (context_id, task_id, message_id) = provenance_ids(provenance.as_ref());
    let message_id = message_id.or_else(|| Some(format!("task-daemon-{}", uuid::Uuid::new_v4())));
    let metadata =
        Some(DispatchMetadata::task_daemon_publish(wire::HOST_SOURCE_RECORDS_V1).into_value());

    let mut event =
        ProducedEvent::host_source_records(source_kind, source_key, payload, message_id, metadata)
            .context("building host.source-records produced event")?;
    event.context_id = context_id;
    event.task_id = task_id;
    Ok(event)
}

fn provenance_ids(
    provenance: Option<&ContractProvenance>,
) -> (
    Option<baml_rt_core::ContextId>,
    Option<baml_rt_core::TaskId>,
    Option<String>,
) {
    let Some(value) = provenance else {
        return (None, None, None);
    };
    (
        value.context_id.clone(),
        value.task_id.clone(),
        value.correlation_id.as_ref().map(|id| id.to_string()),
    )
}
