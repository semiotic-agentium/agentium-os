// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Service-tier authority and client seam.
//!
//! A Service snapshot contains an approved descriptor and an endpoint reference,
//! not executable bytes. The host owns the client and checks the live callable
//! schema before the first invocation. MCP is one implementation of the client
//! seam, supplied by `baml-rt-mcp` so this crate does not depend on it.

use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use serde_json::Value;

use super::invalid;
use crate::{
    ToolName, ToolSessionContext,
    external_tools::{
        DriftGuard, ExternalInvoker, ExternalLifecycleRecorder, ExternalToolSnapshot,
        InvokeRequest, InvokeResponse, ProcessToolHandler, ServiceEndpointRef, ToolDelivery,
        ToolDescribe, ToolSchemaResult, UnifiedToolDescriptor, metadata::build_tool_metadata,
    },
};

/// Client contract for an operator-managed Service endpoint.
///
/// The client owns transport and connection pooling. It must never trust the
/// endpoint's self-reported digest. `describe` returns the live input/output
/// schemas so `DriftGuard` can recompute the digest locally.
#[async_trait]
pub trait ServiceClient: Send + Sync {
    async fn describe(
        &self,
        endpoint: &ServiceEndpointRef,
        tool_name: &str,
        timeout: Duration,
    ) -> Result<ToolSchemaResult>;

    async fn invoke(
        &self,
        endpoint: &ServiceEndpointRef,
        tool_name: &str,
        input: Value,
        timeout: Duration,
    ) -> Result<InvokeResponse>;
}

/// Host-side Service runner. It has no bundle pool and grants no capabilities.
pub struct ServiceHost {
    client: Arc<dyn ServiceClient>,
    describe_timeout: Duration,
    invoke_timeout: Duration,
    recorder: Option<ExternalLifecycleRecorder>,
}

impl ServiceHost {
    pub fn new(
        client: Arc<dyn ServiceClient>,
        describe_timeout: Duration,
        invoke_timeout: Duration,
    ) -> Result<Self> {
        if describe_timeout.is_zero() || invoke_timeout.is_zero() {
            return Err(invalid("service host timeouts must be nonzero"));
        }
        Ok(Self {
            client,
            describe_timeout,
            invoke_timeout,
            recorder: None,
        })
    }

    pub fn with_lifecycle_recorder(mut self, recorder: ExternalLifecycleRecorder) -> Self {
        self.recorder = Some(recorder);
        self
    }

    /// Re-check the endpoint during deploy. The handler repeats this check
    /// lazily on first invoke through DriftGuard.
    pub async fn verify(&self, snapshot: &ExternalToolSnapshot) -> Result<()> {
        if !snapshot.approval.state.is_approved() {
            return Err(invalid("external service is not approved"));
        }
        if let Some(expires_at) = &snapshot.approval.expires_at {
            let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at)
                .map_err(|_| invalid("invalid service approval expiry"))?;
            if expires_at <= chrono::Utc::now() {
                return Err(invalid("external service approval expired"));
            }
        }
        crate::external_tools::validate_external_tool_snapshot(snapshot)?;
        let v2 = snapshot
            .v2
            .as_ref()
            .ok_or_else(|| invalid("service verification requires a v2 snapshot"))?;
        let (endpoint, expected) = match &v2.delivery {
            ToolDelivery::Reference { endpoint, .. } => (
                endpoint,
                snapshot
                    .describe
                    .schema_digest
                    .ok_or_else(|| invalid("service snapshot is missing its schema digest"))?,
            ),
            ToolDelivery::Bundled { .. } => {
                return Err(invalid(
                    "service verification cannot use a bundled snapshot",
                ));
            }
        };
        let tool_name = service_tool_name(&v2.descriptor)?;
        let observed = self
            .client
            .describe(endpoint, &tool_name, self.describe_timeout)
            .await?;
        let observed =
            crate::external_tools::compute_schema_digest(&observed.input, &observed.output);
        if observed != expected {
            return Err(invalid(format!(
                "service '{}' schema drift: approved snapshot digest {}, live endpoint digest {}",
                snapshot.tool.name, expected, observed
            )));
        }
        Ok(())
    }

    /// Build a normal tool handler for an approved Service snapshot.
    pub fn handler(self: &Arc<Self>, snapshot: ExternalToolSnapshot) -> Result<ProcessToolHandler> {
        if !snapshot.approval.state.is_approved() {
            return Err(invalid("external service is not approved"));
        }
        if let Some(expires_at) = &snapshot.approval.expires_at {
            let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at)
                .map_err(|_| invalid("invalid service approval expiry"))?;
            if expires_at <= chrono::Utc::now() {
                return Err(invalid("external service approval expired"));
            }
        }
        crate::external_tools::validate_external_tool_snapshot(&snapshot)?;
        let v2 = snapshot
            .v2
            .as_ref()
            .ok_or_else(|| invalid("service handler requires a v2 snapshot"))?;
        let (endpoint, expected_schema_digest) = match &v2.delivery {
            ToolDelivery::Reference {
                endpoint,
                descriptor_source: _,
            } => (
                endpoint.clone(),
                snapshot
                    .describe
                    .schema_digest
                    .ok_or_else(|| invalid("service snapshot is missing its schema digest"))?,
            ),
            ToolDelivery::Bundled { .. } => {
                return Err(invalid("service handler cannot run a bundled snapshot"));
            }
        };
        let tool_name = ToolName::parse(&snapshot.tool.name)?;
        let service_tool_name = service_tool_name(&v2.descriptor)?;
        let mut metadata = build_tool_metadata(Path::new(""), &snapshot.tool, &tool_name)?;
        metadata.digest = Some(v2.descriptor_digest.to_string());
        let guard = Arc::new(DriftGuard::new(
            tool_name.clone(),
            expected_schema_digest.to_string(),
            self.describe_timeout,
            self.recorder.clone(),
        ));
        let host = self.clone();
        let snapshot = Arc::new(snapshot);
        Ok(ProcessToolHandler::new_scoped(
            metadata,
            Arc::new(move |ctx| {
                Arc::new(ServiceInvoker {
                    host: host.clone(),
                    snapshot: snapshot.clone(),
                    endpoint: endpoint.clone(),
                    service_tool_name: service_tool_name.clone(),
                    ctx,
                })
            }),
            self.invoke_timeout,
        )
        .with_drift_guard(guard))
    }
}

struct ServiceInvoker {
    host: Arc<ServiceHost>,
    snapshot: Arc<ExternalToolSnapshot>,
    endpoint: ServiceEndpointRef,
    service_tool_name: String,
    ctx: ToolSessionContext,
}

#[async_trait]
impl ExternalInvoker for ServiceInvoker {
    async fn describe(&self, _: &ToolName, timeout: Duration) -> Result<ToolDescribe> {
        let schema = self
            .host
            .client
            .describe(
                &self.endpoint,
                &self.service_tool_name,
                timeout.min(self.host.describe_timeout),
            )
            .await?;
        Ok(ToolDescribe {
            protocol_version: "service".to_string(),
            tool_name: self.snapshot.tool.name.clone(),
            supported_methods: vec!["describe".to_string(), "invoke".to_string()],
            max_payload_bytes: None,
            schema_digest: Some(
                crate::external_tools::compute_schema_digest(&schema.input, &schema.output)
                    .to_string(),
            ),
            capabilities: None,
        })
    }

    async fn schema(&self, _: &ToolName, timeout: Duration) -> Result<ToolSchemaResult> {
        self.host
            .client
            .describe(
                &self.endpoint,
                &self.service_tool_name,
                timeout.min(self.host.describe_timeout),
            )
            .await
    }

    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeResponse> {
        if req.tool_name != self.ctx.tool_name || !req.secrets.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "service invocation identity or secret payload is invalid".to_string(),
            ));
        }
        self.host
            .client
            .invoke(
                &self.endpoint,
                &self.service_tool_name,
                req.input,
                req.timeout.min(self.host.invoke_timeout),
            )
            .await
    }
}

fn service_tool_name(descriptor: &UnifiedToolDescriptor) -> Result<String> {
    descriptor
        .name
        .rsplit_once('/')
        .map(|(_, local)| local.to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            invalid(format!(
                "service descriptor name '{}' has no local tool name",
                descriptor.name
            ))
        })
}
