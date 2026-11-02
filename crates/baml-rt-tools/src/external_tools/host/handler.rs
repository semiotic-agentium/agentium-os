// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! V2 invocation through the existing tool session FSM and error taxonomy.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use baml_rt_core::Result;
use baml_sandbox_protocol::protocol_v2::{CallContext, GuestMessage};
use serde_json::json;

use super::{ToolHost, invalid};
use crate::{
    ToolName, ToolSessionContext,
    external_tools::{
        ExternalInvoker, ExternalToolSnapshot, InvokeRequest, InvokeResponse, JsonRpcError,
        ProcessToolHandler, ToolDescribe, ToolSchemaResult, audit_ktool_bundle, map_jsonrpc_error,
        metadata::build_tool_metadata, verify_ktool_snapshot,
    },
};

impl ToolHost {
    pub fn handler(
        self: &Arc<Self>,
        snapshot: ExternalToolSnapshot,
        bytes: Arc<[u8]>,
    ) -> Result<ProcessToolHandler> {
        verify_ktool_snapshot(&audit_ktool_bundle(&bytes)?, &snapshot)?;
        let tool_name = ToolName::parse(&snapshot.tool.name)?;
        let mut metadata = build_tool_metadata(Path::new(""), &snapshot.tool, &tool_name)?;
        let tool_digest = match &snapshot.v2.as_ref().expect("audited v2 snapshot").delivery {
            super::super::ToolDelivery::Bundled { tool_digest, .. } => *tool_digest,
            super::super::ToolDelivery::Reference { .. } => {
                return Err(invalid(
                    "Service snapshots must use ServiceHost, not ToolHost",
                ));
            }
        };
        metadata.digest = Some(tool_digest.to_string());
        let snapshot = Arc::new(snapshot);
        let host = self.clone();
        Ok(ProcessToolHandler::new_scoped(
            metadata,
            Arc::new(move |ctx| {
                Arc::new(HostInvoker {
                    host: host.clone(),
                    snapshot: snapshot.clone(),
                    bytes: bytes.clone(),
                    ctx,
                })
            }),
            self.policy.call_timeout,
        ))
    }
}

struct HostInvoker {
    host: Arc<ToolHost>,
    snapshot: Arc<ExternalToolSnapshot>,
    bytes: Arc<[u8]>,
    ctx: ToolSessionContext,
}

#[async_trait]
impl ExternalInvoker for HostInvoker {
    async fn describe(&self, _: &ToolName, _: Duration) -> Result<ToolDescribe> {
        Ok(ToolDescribe {
            protocol_version: "2".into(),
            tool_name: self.snapshot.tool.name.clone(),
            supported_methods: self.snapshot.describe.supported_methods.clone(),
            max_payload_bytes: self.snapshot.describe.max_payload_bytes,
            schema_digest: self.snapshot.describe.schema_digest.map(|d| d.to_string()),
            capabilities: None,
        })
    }

    async fn schema(&self, _: &ToolName, _: Duration) -> Result<ToolSchemaResult> {
        Err(invalid(
            "v2 schemas come from the audited bundle, not live tool/schema",
        ))
    }

    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeResponse> {
        if req.tool_name != self.ctx.tool_name || !req.secrets.is_empty() {
            return Err(invalid(
                "v2 invocation identity or secret payload is invalid",
            ));
        }
        let deadline_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("invalid host clock"))?
            .saturating_add(req.timeout)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let context = CallContext {
            invocation_id: req.invocation_id,
            deadline_ms,
            ..Default::default()
        };
        match self
            .host
            .invoke(&self.snapshot, &self.bytes, &self.ctx, context, req.input)
            .await?
        {
            GuestMessage::Result {
                ok: true,
                output,
                truncated: false,
                ..
            } => Ok(InvokeResponse {
                output: output.unwrap_or(serde_json::Value::Null),
                done: true,
            }),
            GuestMessage::Result {
                error, error_class, ..
            } => Err(map_jsonrpc_error(
                &req.tool_name,
                &JsonRpcError {
                    code: -32000,
                    message: error.unwrap_or_else(|| "tool output was truncated".into()),
                    data: Some(json!({"error_class": error_class})),
                },
            )),
            _ => Err(invalid("unexpected supervisor result")),
        }
    }
}
