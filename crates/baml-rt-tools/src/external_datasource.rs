// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Runner-owned external datasource raw-mode intake and inbox producer.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    AgentDispatchRoutingKey, BamlRtError, ContextId, EventSchemaVersion, EventSourceKind,
    IngressStore, ProducedEvent, Result,
    event_subscription::EventSourceKey,
    ingress_store::{IngressId, IngressItem},
};
use bytes::Bytes;
use http::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{
    EventProducer, ProducerCheckpoint, ProducerPoll, ToolName, WebhookIntake, WebhookRequest,
    WebhookResponse,
    external_tools::{ExternalDatasourceManifest, ExternalDatasourceMode, ExternalToolMetadata},
};

#[derive(Debug, Clone, Default)]
pub struct DatasourceActivation {
    pub source_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RawDatasourceSpec {
    pub tool_name: ToolName,
    pub key: String,
    pub source_kind: EventSourceKind,
    pub source_key: EventSourceKey,
    pub schema_version: EventSchemaVersion,
    pub response_status: StatusCode,
    pub dedupe_header: Option<String>,
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawIngressEnvelope {
    payload: Value,
    context_id: String,
}

pub fn raw_datasource_spec(
    tool: &ExternalToolMetadata,
    datasource: &ExternalDatasourceManifest,
    activation: &DatasourceActivation,
) -> Result<RawDatasourceSpec> {
    if datasource.mode != ExternalDatasourceMode::Raw {
        return Err(BamlRtError::InvalidArgument(format!(
            "datasource '{}.{}' is not raw mode",
            tool.name, datasource.key
        )));
    }
    if datasource.handler.is_some() {
        return Err(BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' must not set handler",
            tool.name, datasource.key
        )));
    }
    if datasource.kind != "webhook" {
        return Err(BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' kind '{}' is unsupported (expected webhook)",
            tool.name, datasource.key, datasource.kind
        )));
    }
    for method in &datasource.methods {
        if method != "POST" {
            return Err(BamlRtError::InvalidArgument(format!(
                "raw datasource '{}.{}' only supports POST in MVP (got {})",
                tool.name, datasource.key, method
            )));
        }
    }

    let source_kind_raw = match &datasource.source_kind {
        Some(value) => value.clone(),
        None if tool.event_sources.len() == 1 => tool.event_sources[0].clone(),
        None => {
            return Err(BamlRtError::InvalidArgument(format!(
                "raw datasource '{}.{}' must set source_kind unless tool declares exactly one event_sources[] entry",
                tool.name, datasource.key
            )));
        }
    };
    if !tool
        .event_sources
        .iter()
        .any(|value| value == &source_kind_raw)
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' source_kind '{}' is not declared in event_sources[]",
            tool.name, datasource.key, source_kind_raw
        )));
    }
    let source_kind = EventSourceKind::parse(&source_kind_raw).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' has invalid source_kind '{}'",
            tool.name, datasource.key, source_kind_raw
        ))
    })?;

    let schema_version_raw = datasource.schema_version.as_ref().ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' must set schema_version",
            tool.name, datasource.key
        ))
    })?;
    if !tool
        .schemas
        .events
        .iter()
        .any(|event| &event.schema_version == schema_version_raw)
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' schema_version '{}' has no matching event schema",
            tool.name, datasource.key, schema_version_raw
        )));
    }
    let schema_version = EventSchemaVersion::parse(schema_version_raw).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' has invalid schema_version '{}'",
            tool.name, datasource.key, schema_version_raw
        ))
    })?;

    let source_key_raw = activation
        .source_key
        .as_ref()
        .or(datasource.source_key.as_ref())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "raw datasource '{}.{}' must set source_key or activation override",
                tool.name, datasource.key
            ))
        })?;
    let source_key = EventSourceKey::parse(source_key_raw).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' has invalid source_key '{}'",
            tool.name, datasource.key, source_key_raw
        ))
    })?;

    let status = datasource.response_status.unwrap_or(202);
    let response_status = StatusCode::from_u16(status).map_err(|err| {
        BamlRtError::InvalidArgument(format!(
            "raw datasource '{}.{}' has invalid response_status {}: {}",
            tool.name, datasource.key, status, err
        ))
    })?;

    Ok(RawDatasourceSpec {
        tool_name: ToolName::parse(&tool.name)?,
        key: datasource.key.clone(),
        source_kind,
        source_key,
        schema_version,
        response_status,
        dedupe_header: datasource.dedupe_header.clone(),
        max_body_bytes: datasource.max_body_bytes.unwrap_or(1_048_576) as usize,
    })
}

pub struct RawDatasourceIntake {
    spec: RawDatasourceSpec,
    mount_path: String,
    intake_key: String,
    store: Arc<dyn IngressStore>,
}

impl RawDatasourceIntake {
    pub fn new(spec: RawDatasourceSpec, store: Arc<dyn IngressStore>) -> Self {
        let mount_path = format!("/webhooks/ext/{}/{}", spec.tool_name, spec.key);
        let intake_key = format!("external-datasource:{}:{}", spec.tool_name, spec.key);
        Self {
            spec,
            mount_path,
            intake_key,
            store,
        }
    }
}

#[async_trait]
impl WebhookIntake for RawDatasourceIntake {
    fn intake_key(&self) -> &str {
        &self.intake_key
    }

    fn mount_path(&self) -> &str {
        &self.mount_path
    }

    fn methods(&self) -> &[Method] {
        const POST_ONLY: &[Method] = &[Method::POST];
        POST_ONLY
    }

    async fn handle(&self, request: WebhookRequest) -> Result<WebhookResponse> {
        if request.body.len() > self.spec.max_body_bytes {
            return Ok(WebhookResponse::bad_request(format!(
                "webhook body exceeds {} bytes",
                self.spec.max_body_bytes
            )));
        }
        let payload: Value = match serde_json::from_slice(&request.body) {
            Ok(Value::Object(_)) => serde_json::from_slice(&request.body).expect("already parsed"),
            Ok(_) => {
                return Ok(WebhookResponse::bad_request(
                    "webhook body must be a JSON object",
                ));
            }
            Err(err) => return Ok(WebhookResponse::bad_request(format!("invalid JSON: {err}"))),
        };
        let dedupe_value = if let Some(header) = &self.spec.dedupe_header {
            request
                .headers
                .get(header)
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| sha256_hex(&request.body))
        } else {
            sha256_hex(&request.body)
        };
        let ingress_id =
            ingress_id_for(&self.spec.source_kind, &self.spec.source_key, &dedupe_value);
        let context_id = ContextId::from(format!("external-datasource:{}", ingress_id).as_str());
        let envelope = RawIngressEnvelope {
            payload,
            context_id: context_id.to_string(),
        };
        let payload_json = serde_json::to_string(&envelope).map_err(|err| {
            BamlRtError::InvalidArgument(format!("failed to serialize raw ingress envelope: {err}"))
        })?;
        let item = IngressItem {
            ingress_id,
            source_kind: self.spec.source_kind.clone(),
            source_key: self.spec.source_key.clone(),
            payload_json,
            enqueued_at_unix_ms: baml_rt_core::now_unix_ms("external_datasource_ingress"),
        };
        self.store.enqueue(&item).await?;
        Ok(WebhookResponse::new(
            self.spec.response_status,
            Bytes::new(),
        ))
    }
}

pub struct RawDatasourceProducer {
    producer_key: String,
    source_kinds: Vec<EventSourceKind>,
    routing_key: AgentDispatchRoutingKey,
    schema_version: EventSchemaVersion,
    store: Arc<dyn IngressStore>,
}

impl RawDatasourceProducer {
    pub fn new(spec: RawDatasourceSpec, store: Arc<dyn IngressStore>) -> Self {
        let producer_key = format!("external-datasource:{}:{}", spec.tool_name, spec.key);
        let routing_key = AgentDispatchRoutingKey::parse(spec.source_kind.as_str())
            .expect("non-empty source kind is valid routing key");
        Self {
            producer_key,
            source_kinds: vec![spec.source_kind],
            routing_key,
            schema_version: spec.schema_version,
            store,
        }
    }
}

#[async_trait]
impl EventProducer for RawDatasourceProducer {
    fn producer_key(&self) -> &str {
        &self.producer_key
    }

    fn source_kinds(&self) -> &[EventSourceKind] {
        &self.source_kinds
    }

    async fn poll(&self, checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll> {
        let now_ms = baml_rt_core::now_unix_ms("external_datasource_ingress");
        let checkpoint_delivered: Vec<IngressId> = checkpoint
            .value()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        let reconciled = !checkpoint_delivered.is_empty();
        if reconciled {
            self.store
                .mark_delivered(&checkpoint_delivered, now_ms)
                .await?;
        }
        self.store
            .requeue_stale(now_ms.saturating_sub(60_000))
            .await?;

        let pending = self.store.list_pending(&self.source_kinds, 100).await?;
        let pending_ids = pending
            .iter()
            .map(|item| item.ingress_id.clone())
            .collect::<Vec<_>>();
        let emitted = self.store.mark_emitted(&pending_ids, now_ms).await?;
        let emitted_set = emitted
            .iter()
            .map(IngressId::as_str)
            .collect::<std::collections::HashSet<_>>();
        let mut events = Vec::new();
        let mut delivered = Vec::new();
        for item in pending
            .into_iter()
            .filter(|item| emitted_set.contains(item.ingress_id.as_str()))
        {
            match serde_json::from_str::<RawIngressEnvelope>(&item.payload_json) {
                Ok(envelope) => {
                    delivered.push(item.ingress_id.clone());
                    events.push(ProducedEvent {
                        routing_key: self.routing_key.clone(),
                        schema_version: self.schema_version.clone(),
                        source_kind: item.source_kind.clone(),
                        source_key: item.source_key.clone(),
                        messages: vec![envelope.payload],
                        context_id: Some(ContextId::from(envelope.context_id.as_str())),
                        task_id: None,
                        message_id: Some(item.ingress_id.to_string()),
                        producer_key: None,
                        metadata: None,
                    });
                }
                Err(err) => {
                    warn!(ingress_id = %item.ingress_id, error = %err, "dropping malformed raw datasource ingress item");
                    self.store
                        .mark_delivered(std::slice::from_ref(&item.ingress_id), now_ms)
                        .await?;
                }
            }
        }
        let checkpoint = if delivered.is_empty() {
            if reconciled {
                ProducerCheckpoint::some("[]")
            } else {
                ProducerCheckpoint::none()
            }
        } else {
            ProducerCheckpoint::some(serde_json::to_string(&delivered).unwrap_or_default())
        };
        Ok(ProducerPoll { events, checkpoint })
    }
}

fn ingress_id_for(
    source_kind: &EventSourceKind,
    source_key: &EventSourceKey,
    dedupe_value: &str,
) -> IngressId {
    let mut hasher = Sha256::new();
    hasher.update(source_kind.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(source_key.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(dedupe_value.as_bytes());
    IngressId::parse(format!("external-datasource:{:x}", hasher.finalize())).expect("sha256 id")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        ToolAccess,
        external_tools::{EventSchema, ExternalToolManifest, InvocationMode, MetadataSchemas},
        ingress_store::test_support::install_memory_ingress_store,
    };

    fn metadata() -> ExternalToolMetadata {
        ExternalToolManifest {
            tool_abi_version: "1".to_string(),
            name: "support/echo".to_string(),
            description: "echo".to_string(),
            bundle: "support".to_string(),
            local_name: "echo".to_string(),
            access_level: ToolAccess::Read,
            tags: vec![],
            event_sources: vec!["echo".to_string()],
            datasources: vec![],
            invocation_mode: InvocationMode::SingleShot,
            session_policy: Default::default(),
            secrets: vec![],
            secret_scope: Default::default(),
            capabilities: json!({}),
            config_bundle: None,
            runtime: None,
            coordination: None,
        }
        .into_metadata(MetadataSchemas {
            input: Value::Null,
            output: Value::Null,
            events: vec![EventSchema {
                schema_version: "echo.v1".to_string(),
                name: Some("EchoEvent".to_string()),
                schema: json!({"type": "object"}),
            }],
        })
    }

    fn datasource() -> ExternalDatasourceManifest {
        ExternalDatasourceManifest {
            key: "echo-webhook".to_string(),
            kind: "webhook".to_string(),
            mode: ExternalDatasourceMode::Raw,
            source_kind: None,
            schema_version: Some("echo.v1".to_string()),
            source_key: Some("echo:local".to_string()),
            response_status: None,
            dedupe_header: None,
            max_body_bytes: None,
            timeout_ms: None,
            methods: Vec::new(),
            handler: None,
        }
    }

    #[tokio::test]
    async fn raw_intake_enqueues_and_producer_emits_event() {
        let (_guard, store) = install_memory_ingress_store();
        let spec =
            raw_datasource_spec(&metadata(), &datasource(), &DatasourceActivation::default())
                .expect("raw spec");
        let intake = RawDatasourceIntake::new(spec.clone(), store.clone());
        let response = intake
            .handle(WebhookRequest {
                method: Method::POST,
                uri: "/webhooks/ext/support/echo/echo-webhook".parse().unwrap(),
                headers: Default::default(),
                body: Bytes::from_static(br#"{"message":"hello"}"#),
            })
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::ACCEPTED);

        let producer = RawDatasourceProducer::new(spec, store);
        let poll = producer.poll(&ProducerCheckpoint::none()).await.unwrap();
        assert_eq!(poll.events.len(), 1);
        assert_eq!(poll.events[0].source_kind.as_str(), "echo");
        assert_eq!(poll.events[0].source_key.as_str(), "echo:local");
        assert_eq!(poll.events[0].schema_version.as_str(), "echo.v1");
        assert_eq!(poll.events[0].messages[0], json!({"message": "hello"}));
        assert!(poll.events[0].context_id.is_some());
    }
}
