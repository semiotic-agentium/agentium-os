// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! ClickUp polling for task-daemon — delegates lifecycle diffing to `baml_tools_clickup::poll`.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use baml_tools_clickup::{
    ClickupLifecycleRevisionSlot as ToolRevisionSlot, ClickupPollState as ToolPollState,
    ClickupSourceConfig as ToolSourceConfig, ClickupTaskSnapshot as ToolTaskSnapshot,
    poll_clickup_lists,
};
use integrations_clickup_client::ClickUpClient;
use thiserror::Error;

use crate::{
    daemon::{SourcePoll, TaskSource},
    state::{ClickupLifecycleRevisionSlot, ClickupTaskSnapshot, TaskDaemonState},
};

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
    fn to_tool_config(&self) -> std::result::Result<ToolSourceConfig, ClickupSourceConfigError> {
        let list_ids: Vec<String> = self
            .list_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if list_ids.is_empty() {
            return Err(ClickupSourceConfigError::MissingListIds);
        }
        Ok(ToolSourceConfig { list_ids })
    }
}

#[derive(Clone)]
/// ClickUp-backed task source that emits raw lifecycle event records.
pub struct ClickupTaskSource {
    client: ClickUpClient,
    config: ToolSourceConfig,
}

impl ClickupTaskSource {
    /// Creates a ClickUp source with the given configuration.
    pub fn new(
        client: ClickUpClient,
        config: ClickupSourceConfig,
    ) -> std::result::Result<Self, ClickupSourceConfigError> {
        Ok(Self {
            client,
            config: config.to_tool_config()?,
        })
    }

    fn tool_poll_state(source_state: &crate::state::SourceState) -> ToolPollState {
        let task_snapshot = source_state
            .clickup_task_snapshot
            .iter()
            .map(|(id, snap)| {
                (
                    id.clone(),
                    ToolTaskSnapshot {
                        list_id: snap.list_id.clone(),
                        name: snap.name.clone(),
                        status: snap.status.clone(),
                        url: snap.url.clone(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let lifecycle_revisions = source_state
            .clickup_lifecycle_revisions
            .iter()
            .map(|(slot, rev)| (ToolRevisionSlot::new(slot.as_str().to_string()), *rev))
            .collect();
        ToolPollState {
            task_snapshot,
            lifecycle_revisions,
        }
    }

    fn persist_tool_poll_state(
        source_state: &mut crate::state::SourceState,
        tool_state: &ToolPollState,
    ) {
        source_state.clickup_task_snapshot = tool_state
            .task_snapshot
            .iter()
            .map(|(id, snap)| {
                (
                    id.clone(),
                    ClickupTaskSnapshot {
                        list_id: snap.list_id.clone(),
                        name: snap.name.clone(),
                        status: snap.status.clone(),
                        url: snap.url.clone(),
                    },
                )
            })
            .collect();
        source_state.clickup_lifecycle_revisions = tool_state
            .lifecycle_revisions
            .iter()
            .map(|(slot, rev)| {
                (
                    ClickupLifecycleRevisionSlot::new(slot.as_str().to_string()),
                    *rev,
                )
            })
            .collect();
    }
}

#[async_trait]
impl TaskSource for ClickupTaskSource {
    fn source_key(&self) -> String {
        ToolSourceConfig::source_key(&self.config.list_ids)
    }

    async fn poll(&mut self, state: &mut TaskDaemonState) -> Result<SourcePoll> {
        let source_key = self.source_key();
        let previous = state.source_state(&source_key).cloned().unwrap_or_default();
        let tool_previous = Self::tool_poll_state(&previous);

        let outcome = poll_clickup_lists(&self.client, &self.config, tool_previous)
            .await
            .context("polling ClickUp lists")?;

        let source_state = state.source_state_mut(&source_key);
        Self::persist_tool_poll_state(source_state, &outcome.state);

        Ok(SourcePoll::clickup(
            outcome.source_key,
            outcome.source_label,
            outcome.lifecycle_events,
            outcome.items_scanned,
        ))
    }
}
