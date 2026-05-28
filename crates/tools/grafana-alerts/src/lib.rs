//! `support/grafana-alerts` — Grafana alert webhook intake and event source.
//!
//! This tool is **webhook-only** from an agent's perspective: agents do not
//! invoke it. They subscribe to `grafana.alert.v1` events emitted by the
//! [`GrafanaAlertEventProducer`]. The runner-served webhook route enqueues
//! incoming payloads into the shared [`IngressStore`](baml_rt_core::IngressStore)
//! via [`webhook::enqueue_webhook`]; the producer drains it and dispatches.
//!
//! The `Status` operation is provided for operator/CLI inspection only —
//! agents that try to use it as a regular tool will get a clear error.

pub mod intake;
pub mod mapping;
pub mod producer;
pub mod webhook;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{baml_tool, bundles::Support, tools::BamlTool};
pub use intake::{
    GRAFANA_INTAKE_KEY, GRAFANA_WEBHOOK_PATH, GrafanaAlertsConfig, GrafanaWebhookIntake,
};
pub use mapping::{AlertIdentity, AlertStatus, ContextResolution, MappingStore};
pub use producer::{
    GRAFANA_ALERT_SCHEMA_VERSION, GRAFANA_INBOX_PRODUCER_KEY, GRAFANA_ROUTING_KEY,
    GRAFANA_SOURCE_KIND, GrafanaAlertEventProducer,
};
use serde::{Deserialize, Serialize};
pub use webhook::{
    DEFAULT_SOURCE_KEY, EnqueueOutcome, GrafanaAlert, GrafanaIngressEnvelope,
    GrafanaWebhookPayload, enqueue_webhook,
};

/// Inspection input — exposed so the tool has a valid `BamlTool::Input` type.
/// Agents that invoke this will receive an `InvalidArgument` error explaining
/// the tool is webhook-driven.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct GrafanaAlertsStatusInput {}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct GrafanaAlertsStatusOutput {
    pub message: String,
}

impl baml_rt_tools::DescribeAction for GrafanaAlertsStatusInput {
    fn describe(&self) -> String {
        "inspecting Grafana alerts intake".to_string()
    }
}

#[derive(Default)]
pub struct GrafanaAlertsTool;

#[baml_tool(
    name = "support/grafana-alerts",
    description = "Grafana alert webhook intake. Operates as a host-managed event source; agents subscribe to grafana.alert.v1 rather than invoking this tool directly.",
    tags = ["support", "grafana", "alerts", "events"],
    access = Read,
    event_sources = ["grafana"],
    baml_types = [GrafanaAlertsStatusInput, GrafanaAlertsStatusOutput],
)]
#[async_trait]
impl BamlTool for GrafanaAlertsTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "grafana-alerts";
    type OpenInput = ();
    type Input = GrafanaAlertsStatusInput;
    type Output = GrafanaAlertsStatusOutput;

    fn description(&self) -> &'static str {
        "Grafana alert intake (webhook-driven). Subscribe to grafana.alert.v1 events; do not call directly."
    }

    fn describe_open(&self) -> String {
        "opening Grafana alerts intake (webhook-driven)".to_string()
    }

    async fn execute(&self, _args: Self::Input) -> Result<Self::Output> {
        Err(BamlRtError::InvalidArgument(
            "support/grafana-alerts is webhook-driven; subscribe to grafana.alert.v1 instead of invoking it".to_string(),
        ))
    }
}
