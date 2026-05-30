// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `WebhookIntake` implementation for `support/grafana-alerts`.
//!
//! Mounts `POST /webhooks/grafana` on the runner's public HTTP surface,
//! parses Alertmanager-shape payloads, resolves `context_id` via the
//! mapping store, and enqueues one [`IngressItem`] per alert. The
//! companion [`GrafanaAlertEventProducer`](crate::GrafanaAlertEventProducer)
//! drains the store and emits `grafana.alert.v1` dispatch events.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result, clock_events};
use baml_rt_tools::{
    WebhookAuthTier, WebhookIntake, WebhookIntakeBuildContext, WebhookIntakeBuildFuture,
    WebhookIntakeProvider, WebhookRequest, WebhookResponse, ingress_store::ingress_store,
};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

use crate::{
    mapping::MappingStore,
    webhook::{DEFAULT_SOURCE_KEY, GrafanaWebhookPayload, enqueue_webhook},
};

/// Configuration for the Grafana alerts intake.
///
/// - `source_key` — the [`EventSourceKey`](baml_rt_core::EventSourceKey)
///   string assigned to events emitted by this intake. Defaults to
///   `grafana:local` so the demo and CI work without explicit config.
/// - `sqlite_path` — filesystem path for the fingerprint→`context_id`
///   mapping database. When absent the mapping is held in memory only,
///   which means a runner restart loses firing/resolved continuity. The
///   demo Helm chart should pass a PVC-backed path.
#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct GrafanaAlertsConfig {
    #[serde(default)]
    pub source_key: Option<String>,
    #[serde(default)]
    pub sqlite_path: Option<String>,
}

pub const GRAFANA_WEBHOOK_PATH: &str = "/webhooks/grafana";
pub const GRAFANA_INTAKE_KEY: &str = "support/grafana-alerts";

/// Webhook intake that turns Grafana Alertmanager POSTs into enqueued
/// [`IngressItem`]s for the inbox-draining producer to dispatch.
pub struct GrafanaWebhookIntake {
    mapping: Arc<MappingStore>,
    source_key: String,
}

impl GrafanaWebhookIntake {
    pub fn new(mapping: Arc<MappingStore>, source_key: String) -> Self {
        Self {
            mapping,
            source_key,
        }
    }
}

#[async_trait]
impl WebhookIntake for GrafanaWebhookIntake {
    fn intake_key(&self) -> &str {
        GRAFANA_INTAKE_KEY
    }

    fn mount_path(&self) -> &str {
        GRAFANA_WEBHOOK_PATH
    }

    fn auth_tier(&self) -> WebhookAuthTier {
        // Grafana cannot present an operator token; the path is public
        // inside the cluster network, same posture as /chat and /dispatch.
        WebhookAuthTier::Public
    }

    async fn handle(&self, request: WebhookRequest) -> Result<WebhookResponse> {
        let payload: GrafanaWebhookPayload = match serde_json::from_slice(&request.body) {
            Ok(value) => value,
            Err(err) => {
                warn!(error = %err, "grafana webhook body did not deserialize");
                return Ok(WebhookResponse::bad_request(format!(
                    "invalid Grafana webhook body: {err}"
                )));
            }
        };

        let Some(store) = ingress_store() else {
            error!("grafana webhook received but ingress store is not installed");
            return Ok(WebhookResponse::internal_error(
                "ingress store not installed",
            ));
        };

        let now_ms = baml_rt_core::now_unix_ms(clock_events::GRAFANA_INGRESS);
        let outcome = enqueue_webhook(
            &payload,
            self.mapping.as_ref(),
            store.as_ref(),
            &self.source_key,
            now_ms,
        )
        .await?;

        WebhookResponse::json(
            StatusCode::ACCEPTED,
            &serde_json::json!({
                "enqueued": outcome.enqueued,
                "duplicates": outcome.duplicates,
                "skipped": outcome.skipped,
            }),
        )
    }
}

fn build_intakes(ctx: WebhookIntakeBuildContext) -> WebhookIntakeBuildFuture {
    Box::pin(async move {
        let config: GrafanaAlertsConfig = match ctx.config {
            Some(value) => serde_json::from_value(value).map_err(|err| {
                BamlRtError::InvalidArgument(format!(
                    "invalid config for support/grafana-alerts intake: {err}"
                ))
            })?,
            None => GrafanaAlertsConfig::default(),
        };

        let source_key = config
            .source_key
            .unwrap_or_else(|| DEFAULT_SOURCE_KEY.to_string());

        let mapping = match config.sqlite_path.as_deref() {
            Some(path) => {
                let path = PathBuf::from(path);
                MappingStore::open(&path).map_err(|err| {
                    BamlRtError::InvalidArgument(format!(
                        "grafana-alerts mapping store failed to open '{}': {err}",
                        path.display()
                    ))
                })?
            }
            None => {
                warn!(
                    "support/grafana-alerts has no sqlite_path configured; firing/resolved \
                     continuity will not survive runner restarts"
                );
                MappingStore::open_in_memory().map_err(|err| {
                    BamlRtError::InvalidArgument(format!(
                        "grafana-alerts in-memory mapping store init failed: {err}"
                    ))
                })?
            }
        };

        let intake: Arc<dyn WebhookIntake> =
            Arc::new(GrafanaWebhookIntake::new(Arc::new(mapping), source_key));
        Ok(vec![intake])
    })
}

inventory::submit! {
    WebhookIntakeProvider {
        tool_name: "support/grafana-alerts",
        build: build_intakes,
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, sync::Mutex};

    use baml_rt_tools::{
        WebhookAuthTier, WebhookIntake, WebhookRequest,
        ingress_store::test_support::install_memory_ingress_store,
    };
    use bytes::Bytes;
    use http::{HeaderMap, Method, StatusCode, Uri};

    use super::*;

    // Intake tests mutate the process-wide ingress store; serialize them.
    fn test_lock() -> &'static Mutex<()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn run_serial_test(test: impl Future<Output = ()>) {
        let _guard = test_lock().lock().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(test);
    }

    fn empty_request(body: &str) -> WebhookRequest {
        WebhookRequest {
            method: Method::POST,
            uri: Uri::from_static("/webhooks/grafana"),
            headers: HeaderMap::new(),
            body: Bytes::copy_from_slice(body.as_bytes()),
        }
    }

    fn intake() -> GrafanaWebhookIntake {
        let mapping = MappingStore::open_in_memory().expect("mapping");
        GrafanaWebhookIntake::new(Arc::new(mapping), DEFAULT_SOURCE_KEY.to_string())
    }

    #[tokio::test]
    async fn declares_public_post_route_at_canonical_path() {
        let intake = intake();
        assert_eq!(intake.mount_path(), "/webhooks/grafana");
        assert_eq!(intake.auth_tier(), WebhookAuthTier::Public);
        assert_eq!(intake.methods(), &[Method::POST]);
        assert_eq!(intake.intake_key(), "support/grafana-alerts");
    }

    #[test]
    fn malformed_body_returns_bad_request() {
        run_serial_test(async {
            let (_store_guard, _store) = install_memory_ingress_store();
            let response = intake().handle(empty_request("not json")).await.unwrap();
            assert_eq!(response.status, StatusCode::BAD_REQUEST);
        });
    }

    #[test]
    fn missing_ingress_store_returns_internal_error() {
        run_serial_test(async {
            // Do not install an ingress store. The handler must report 500.
            baml_rt_tools::ingress_store::clear_ingress_store();
            let body = serde_json::json!({
                "status": "firing",
                "groupKey": "g1",
                "alerts": [{
                    "status": "firing",
                    "fingerprint": "fp1",
                    "startsAt": "2026-05-25T12:00:00Z",
                }]
            })
            .to_string();
            let response = intake().handle(empty_request(&body)).await.unwrap();
            assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
        });
    }

    #[test]
    fn well_formed_payload_returns_accepted_and_enqueues() {
        run_serial_test(async {
            let (_store_guard, store) = install_memory_ingress_store();
            let body = serde_json::json!({
                "status": "firing",
                "groupKey": "g1",
                "alerts": [{
                    "status": "firing",
                    "fingerprint": "fp1",
                    "startsAt": "2026-05-25T12:00:00Z",
                    "labels": {"alertname": "HighLatency"},
                }]
            })
            .to_string();
            let response = intake().handle(empty_request(&body)).await.unwrap();
            assert_eq!(response.status, StatusCode::ACCEPTED);
            let pending = baml_rt_core::IngressStore::list_pending(store.as_ref(), 100)
                .await
                .unwrap();
            assert_eq!(pending.len(), 1);
        });
    }
}
