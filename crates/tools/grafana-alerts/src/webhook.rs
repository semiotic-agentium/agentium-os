// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Grafana alert webhook intake — parses Alertmanager-shape payloads,
//! resolves Agentium `context_id` via the mapping store, and enqueues an
//! [`IngressItem`] per alert for the producer to drain.

use baml_rt_core::{
    BamlRtError, Result,
    event_subscription::EventSourceKey,
    ingress_store::{IngressId, IngressItem, IngressStore},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::mapping::{AlertIdentity, AlertStatus, MappingStore};

/// One alert from a Grafana / Alertmanager webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrafanaAlert {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub labels: serde_json::Value,
    #[serde(default)]
    pub annotations: serde_json::Value,
    #[serde(default, rename = "startsAt")]
    pub starts_at: Option<String>,
    #[serde(default, rename = "endsAt")]
    pub ends_at: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default, rename = "dashboardURL")]
    pub dashboard_url: Option<String>,
    #[serde(default, rename = "panelURL")]
    pub panel_url: Option<String>,
}

/// Grafana / Alertmanager webhook envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrafanaWebhookPayload {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "groupKey")]
    pub group_key: Option<String>,
    #[serde(default)]
    pub receiver: Option<String>,
    #[serde(default)]
    pub alerts: Vec<GrafanaAlert>,
}

/// Persisted ingress envelope. The producer reconstructs the dispatch event
/// from this; resolving `context_id` at intake time (not poll time) preserves
/// firing/resolved continuity even if the producer drains long after.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrafanaIngressEnvelope {
    pub context_id: String,
    pub message_id: String,
    pub status: String,
    pub fingerprint: String,
    pub group_key: String,
    pub source_key: String,
    pub alert: GrafanaAlert,
    pub group_status: Option<String>,
    pub receiver: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnqueueOutcome {
    pub enqueued: usize,
    pub duplicates: usize,
    pub skipped: usize,
}

pub const DEFAULT_SOURCE_KEY: &str = "grafana:local";

fn alert_status(alert: &GrafanaAlert, group: &GrafanaWebhookPayload) -> AlertStatus {
    let raw = alert
        .status
        .as_deref()
        .or(group.status.as_deref())
        .unwrap_or("firing")
        .to_ascii_lowercase();
    if raw == "resolved" {
        AlertStatus::Resolved
    } else {
        AlertStatus::Firing
    }
}

fn ingress_id_for(message_id: &str) -> IngressId {
    let mut hasher = Sha256::new();
    hasher.update(message_id.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    IngressId::parse(format!("grafana-alerts:{hex}")).expect("non-empty sha256-derived ingress id")
}

/// Parse a Grafana webhook body, resolve each alert's `context_id`, and
/// enqueue one [`IngressItem`] per alert. Idempotent: the ingress_id is
/// derived from `(fingerprint, status, startsAt)` so retransmits are a no-op.
pub async fn enqueue_webhook(
    payload: &GrafanaWebhookPayload,
    mapping: &MappingStore,
    store: &dyn IngressStore,
    source_key_raw: &str,
    now_ms: u64,
) -> Result<EnqueueOutcome> {
    let source_key = EventSourceKey::parse(source_key_raw).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!("invalid Grafana source key '{source_key_raw}'"))
    })?;
    let group_key = payload.group_key.clone().unwrap_or_default();
    let mut outcome = EnqueueOutcome {
        enqueued: 0,
        duplicates: 0,
        skipped: 0,
    };

    if payload.alerts.is_empty() {
        warn!("Grafana webhook payload contained no alerts");
        return Ok(outcome);
    }

    for alert in &payload.alerts {
        let Some(fingerprint) = alert.fingerprint.clone() else {
            warn!("Grafana alert missing fingerprint; skipping");
            outcome.skipped += 1;
            continue;
        };
        let status = alert_status(alert, payload);
        let identity = AlertIdentity {
            fingerprint: fingerprint.clone(),
            group_key: group_key.clone(),
        };
        let resolution = mapping.resolve(&identity, status, now_ms).map_err(|err| {
            BamlRtError::ToolExecution(format!(
                "grafana-alerts mapping resolve failed for fingerprint '{fingerprint}': {err}"
            ))
        })?;
        let starts_at = alert.starts_at.as_deref().unwrap_or("unknown");
        let message_id = format!(
            "grafana:{fingerprint}:{status}:{starts_at}",
            status = status.as_str()
        );
        let envelope = GrafanaIngressEnvelope {
            context_id: resolution.context_id,
            message_id: message_id.clone(),
            status: status.as_str().to_string(),
            fingerprint: fingerprint.clone(),
            group_key: group_key.clone(),
            source_key: source_key_raw.to_string(),
            alert: alert.clone(),
            group_status: payload.status.clone(),
            receiver: payload.receiver.clone(),
        };
        let payload_json = serde_json::to_string(&envelope).map_err(|err| {
            BamlRtError::InvalidArgument(format!(
                "failed to serialize Grafana ingress envelope: {err}"
            ))
        })?;
        let ingress_id = ingress_id_for(&message_id);
        let item = IngressItem {
            ingress_id: ingress_id.clone(),
            source_key: source_key.clone(),
            payload_json,
            enqueued_at_unix_ms: now_ms,
        };
        let inserted = store.enqueue(&item).await?;
        if inserted {
            outcome.enqueued += 1;
            debug!(
                ingress_id = %ingress_id,
                fingerprint = %fingerprint,
                status = status.as_str(),
                "enqueued Grafana alert"
            );
        } else {
            outcome.duplicates += 1;
        }
    }

    Ok(outcome)
}
