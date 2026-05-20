// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Outputs task-daemon can write or deliver.

use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Result;
use async_trait::async_trait;
use baml_rt_core::ProducedEvent;
use integrations_clickup_client::ClickUpClient;
use integrations_github_client::GitHubClient;
use thiserror::Error;

use crate::model::TaskSourceKind;

#[async_trait]
pub trait TaskSink: Send {
    fn name(&self) -> &'static str;
    fn accepts_source(&self, _source: TaskSourceKind) -> bool {
        true
    }
    async fn deliver(&mut self, event: &ProducedEvent) -> Result<()>;
}

pub struct SourceFilteredSink {
    inner: Box<dyn TaskSink>,
    allowed_sources: BTreeSet<TaskSourceKind>,
}

impl SourceFilteredSink {
    pub fn new(inner: Box<dyn TaskSink>, allowed_sources: Vec<TaskSourceKind>) -> Self {
        Self {
            inner,
            allowed_sources: allowed_sources.into_iter().collect(),
        }
    }
}

#[async_trait]
impl TaskSink for SourceFilteredSink {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn accepts_source(&self, source: TaskSourceKind) -> bool {
        self.allowed_sources.contains(&source) && self.inner.accepts_source(source)
    }

    async fn deliver(&mut self, event: &ProducedEvent) -> Result<()> {
        self.inner.deliver(event).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkDeliveryMode {
    DryRun,
    Live,
}

impl SinkDeliveryMode {
    pub fn from_live_flag(live: bool) -> Self {
        if live { Self::Live } else { Self::DryRun }
    }
}

#[derive(Debug, Error)]
pub enum SinkConstructorError {
    #[error("clickup list_id must not be empty")]
    EmptyClickupListId,
    #[error("github owner must not be empty")]
    EmptyGithubOwner,
    #[error("github repo must not be empty")]
    EmptyGithubRepo,
    #[error("agent host base URL must not be empty")]
    EmptyDispatchBaseUrl,
    #[error("agent host base URL is invalid: {raw}")]
    InvalidDispatchBaseUrl { raw: String },
}

#[derive(Debug, Error)]
pub enum SinkDeliveryError {
    #[error("serializing produced event for stdout sink failed")]
    StdoutSerialize(#[source] serde_json::Error),
    #[error("serializing produced event to jsonl failed")]
    JsonlSerialize(#[source] serde_json::Error),
    #[error("jsonl sink I/O failed for {path}: {source}")]
    JsonlIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub struct StdoutSink {
    pretty: bool,
}

impl StdoutSink {
    pub fn new(pretty: bool) -> Self {
        Self { pretty }
    }
}

#[async_trait]
impl TaskSink for StdoutSink {
    fn name(&self) -> &'static str {
        "stdout"
    }

    async fn deliver(&mut self, event: &ProducedEvent) -> Result<()> {
        let serialized = if self.pretty {
            serde_json::to_string_pretty(event)
        } else {
            serde_json::to_string(event)
        }
        .map_err(SinkDeliveryError::StdoutSerialize)?;
        println!("{serialized}");
        Ok(())
    }
}

pub struct JsonlFileSink {
    path: PathBuf,
}

impl JsonlFileSink {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }
}

#[async_trait]
impl TaskSink for JsonlFileSink {
    fn name(&self) -> &'static str {
        "jsonl"
    }

    async fn deliver(&mut self, event: &ProducedEvent) -> Result<()> {
        let path = self.path.clone();
        let event = event.clone();
        tokio::task::spawn_blocking(move || jsonl_deliver_blocking(&path, &event))
            .await
            .map_err(|e| {
                if e.is_panic() {
                    std::panic::resume_unwind(e.into_panic());
                }
                anyhow!("jsonl sink deliver blocking task failed: {e}")
            })??;
        Ok(())
    }
}

fn jsonl_deliver_blocking(
    path: &Path,
    event: &ProducedEvent,
) -> std::result::Result<(), SinkDeliveryError> {
    let line = serde_json::to_string(event).map_err(SinkDeliveryError::JsonlSerialize)?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| SinkDeliveryError::JsonlIo {
            path: path.to_path_buf(),
            source,
        })?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| SinkDeliveryError::JsonlIo {
            path: path.to_path_buf(),
            source,
        })?;
    writeln!(file, "{line}").map_err(|source| SinkDeliveryError::JsonlIo {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all()
        .map_err(|source| SinkDeliveryError::JsonlIo {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}


pub struct ClickUpSink {
    list_id: String,
    mode: SinkDeliveryMode,
}

impl ClickUpSink {
    pub fn new(
        _client: ClickUpClient,
        list_id: String,
        mode: SinkDeliveryMode,
    ) -> std::result::Result<Self, SinkConstructorError> {
        let list_id = list_id.trim().to_string();
        if list_id.is_empty() {
            return Err(SinkConstructorError::EmptyClickupListId);
        }
        Ok(Self { list_id, mode })
    }
}

#[async_trait]
impl TaskSink for ClickUpSink {
    fn name(&self) -> &'static str {
        match self.mode {
            SinkDeliveryMode::DryRun => "clickup-dry-run",
            SinkDeliveryMode::Live => "clickup",
        }
    }

    fn accepts_source(&self, source: TaskSourceKind) -> bool {
        !matches!(source, TaskSourceKind::Clickup)
    }

    async fn deliver(&mut self, _event: &ProducedEvent) -> Result<()> {
        tracing::debug!(
            list_id = %self.list_id,
            "ClickUp external-task sink skipped (source-records + agent publish path)"
        );
        Ok(())
    }
}

pub struct GithubIssueSink {
    owner: String,
    repo: String,
    mode: SinkDeliveryMode,
}

impl GithubIssueSink {
    pub fn new(
        _client: GitHubClient,
        owner: String,
        repo: String,
        mode: SinkDeliveryMode,
    ) -> std::result::Result<Self, SinkConstructorError> {
        let owner = owner.trim().to_string();
        let repo = repo.trim().to_string();
        if owner.is_empty() {
            return Err(SinkConstructorError::EmptyGithubOwner);
        }
        if repo.is_empty() {
            return Err(SinkConstructorError::EmptyGithubRepo);
        }
        Ok(Self { owner, repo, mode })
    }
}

#[async_trait]
impl TaskSink for GithubIssueSink {
    fn name(&self) -> &'static str {
        match self.mode {
            SinkDeliveryMode::DryRun => "github-dry-run",
            SinkDeliveryMode::Live => "github",
        }
    }

    async fn deliver(&mut self, _event: &ProducedEvent) -> Result<()> {
        tracing::debug!(
            owner = %self.owner,
            repo = %self.repo,
            "GitHub issue sink skipped (source-records + agent publish path)"
        );
        Ok(())
    }
}
