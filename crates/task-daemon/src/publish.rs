// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! HTTP publish sink: delivers [`ProducedEvent`] to the runner `POST /events/publish`.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use baml_rt_core::ProducedEvent;
use reqwest::Url;

use crate::sink::{SinkConstructorError, SinkDeliveryMode, TaskSink};

const PUBLISH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub struct PublishSink {
    publish_url: Url,
    client: reqwest::Client,
    mode: SinkDeliveryMode,
}

impl PublishSink {
    pub fn new(base_url: String, mode: SinkDeliveryMode) -> Result<Self, SinkConstructorError> {
        let base = base_url.trim();
        if base.is_empty() {
            return Err(SinkConstructorError::EmptyDispatchBaseUrl);
        }
        let mut publish_url =
            Url::parse(base).map_err(|_| SinkConstructorError::InvalidDispatchBaseUrl {
                raw: base.to_string(),
            })?;
        if !matches!(publish_url.scheme(), "http" | "https") {
            return Err(SinkConstructorError::InvalidDispatchBaseUrl {
                raw: publish_url.to_string(),
            });
        }
        let raw_for_err = publish_url.to_string();
        publish_url
            .path_segments_mut()
            .map_err(|_| SinkConstructorError::InvalidDispatchBaseUrl { raw: raw_for_err })?
            .pop_if_empty()
            .push("events")
            .push("publish");
        let client = reqwest::Client::builder()
            .timeout(PUBLISH_HTTP_TIMEOUT)
            .build()
            .map_err(|e| SinkConstructorError::InvalidDispatchBaseUrl {
                raw: format!("building publish HTTP client: {e}"),
            })?;
        Ok(Self {
            publish_url,
            client,
            mode,
        })
    }
}

#[async_trait]
impl TaskSink for PublishSink {
    fn name(&self) -> &'static str {
        "publish"
    }

    async fn deliver(&mut self, event: &ProducedEvent) -> Result<()> {
        match self.mode {
            SinkDeliveryMode::DryRun => {
                tracing::info!(
                    source_key = %event.source_key,
                    schema = %event.schema_version,
                    "dry-run publish sink skipped HTTP publish"
                );
                return Ok(());
            }
            SinkDeliveryMode::Live => {}
        }
        let body = serde_json::to_value(event).context("serializing produced event")?;
        self.client
            .post(self.publish_url.clone())
            .json(&body)
            .send()
            .await
            .context("POST /events/publish")?
            .error_for_status()
            .context("publish response status")?;
        tracing::info!(
            source_key = %event.source_key,
            context_id = ?event.context_id,
            "published host.source-records event"
        );
        Ok(())
    }
}
