// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Build [`ProducedEvent`] payloads for host pub/sub (`host.source-records.v1`).

use anyhow::{Context, Result, anyhow};
use baml_rt_core::{
    DispatchMetadata, EventSourceKind, HostPollLineage, ProducedEvent, clock_events,
    event_subscription::EventSourceKey, host_wire::wire,
};
use baml_tools_clickup::{ClickupProjectContext, batch_from_lifecycle_events};
use baml_tools_github::{GithubIssuesProjectContext, batch_from_issue_events};
use baml_tools_slack::{normalize::normalize_polling_batch, slack_history_row_value};
use serde_json::json;

use crate::{
    daemon::SourcePoll,
    model::{ProjectContext, SlackMessage, TaskSourceKind},
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

fn slack_messages_to_values(messages: &[SlackMessage]) -> Vec<serde_json::Value> {
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

/// Convert one polled source window into a host-routable [`ProducedEvent`].
pub fn poll_to_produced_event(
    poll: &SourcePoll,
    project: &ProjectContext,
    lineage: &HostPollLineage,
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
            let batch = batch_from_lifecycle_events(
                &poll.source_key,
                &poll.source_label,
                Some(clickup_project_context(project)),
                poll.clickup_lifecycle_events(),
                emitted_at_unix,
            );
            serde_json::to_value(&batch).context("serializing clickup source-records batch")?
        }
        TaskSourceKind::GithubIssues => {
            let batch = batch_from_issue_events(
                &poll.source_key,
                &poll.source_label,
                Some(github_project_context(project)),
                &[],
                emitted_at_unix,
            );
            serde_json::to_value(&batch)
                .context("serializing github_issues source-records batch")?
        }
    };

    let mut metadata =
        DispatchMetadata::task_daemon_publish(wire::HOST_SOURCE_RECORDS_V1).into_value();
    if let Some(obj) = metadata.as_object_mut() {
        if let Some(cursor) = &lineage.source_cursor {
            obj.insert("source_cursor".into(), json!(cursor));
        }
        if !lineage.source_message_ts.is_empty() {
            obj.insert("source_message_ts".into(), json!(lineage.source_message_ts));
        }
    }

    let mut event = ProducedEvent::host_source_records(
        source_kind,
        source_key,
        payload,
        Some(lineage.poll_batch_id.clone()),
        Some(metadata),
    )
    .context("building host.source-records produced event")?;
    event.context_id = Some(lineage.context_id.clone());
    Ok(event)
}
