// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! MCP implementation of the external-tools v2 ServiceClient seam.

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    external_tools::{
        DescribeProvenance, InvokeResponse, ServiceClient, ServiceEndpointRef, ToolSchemaResult,
        compute_descriptor_schema_digest,
    },
    mcp_config::HttpNetworkPolicyConfig,
    mcp_snapshot::canonical_digest,
};
use serde_json::Value;

use crate::runtime::{HttpLaunchConfig, LaunchKind, McpConnection, ServerLaunch};

/// A ServiceClient bound to one MCP Streamable HTTP endpoint.
///
/// The connection performs the normal MCP initialize and tools/list checks. On
/// every first external-tool invoke, the generic ServiceHost asks this client
/// for the live descriptor, and the shared DriftGuard compares its schemas.
pub struct McpServiceClient {
    endpoint: ServiceEndpointRef,
    tool_name: String,
    connection: Arc<McpConnection>,
}

impl McpServiceClient {
    pub fn from_http(
        endpoint: ServiceEndpointRef,
        tool_name: impl Into<String>,
        source: &DescribeProvenance,
    ) -> Result<Self> {
        let url = match &endpoint {
            ServiceEndpointRef::Mcp { url } => url,
            _ => {
                return Err(BamlRtError::InvalidArgument(
                    "unsupported MCP Service endpoint kind".to_string(),
                ));
            }
        };
        let DescribeProvenance::McpToolsList {
            protocol_version,
            server_identity_digest,
            tools_digest,
            ..
        } = source;
        if url.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "MCP Service endpoint URL must not be empty".to_string(),
            ));
        }
        let server_id = url.clone();
        let launch = ServerLaunch {
            server_id,
            startup_timeout: Duration::from_secs(30),
            call_timeout: Duration::from_secs(120),
            server_config_digest: canonical_digest(&endpoint),
            protocol_version: protocol_version.clone(),
            expected_identity_digest: *server_identity_digest,
            expected_tools_digest: *tools_digest,
            cache_root: PathBuf::from(".karakara-os"),
            kind: LaunchKind::Http(HttpLaunchConfig {
                url: url.clone(),
                static_headers: Vec::new(),
                resolved_secrets: Vec::new(),
                network_policy: HttpNetworkPolicyConfig::default(),
                connect_timeout: Duration::from_secs(10),
                request_timeout: Duration::from_secs(120),
                idle_stream_timeout: Duration::from_secs(120),
                max_idle_per_host: 8,
                extra_ca_certs_pem: Vec::new(),
            }),
        };
        Ok(Self {
            endpoint,
            tool_name: tool_name.into(),
            connection: Arc::new(McpConnection::new(launch)),
        })
    }

    pub fn new(
        endpoint: ServiceEndpointRef,
        tool_name: impl Into<String>,
        connection: Arc<McpConnection>,
    ) -> Self {
        Self {
            endpoint,
            tool_name: tool_name.into(),
            connection,
        }
    }

    fn check_target(&self, endpoint: &ServiceEndpointRef, tool_name: &str) -> Result<()> {
        if endpoint != &self.endpoint || tool_name != self.tool_name {
            return Err(BamlRtError::InvalidArgument(
                "MCP ServiceClient received an unexpected endpoint or tool name".to_string(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ServiceClient for McpServiceClient {
    async fn describe(
        &self,
        endpoint: &ServiceEndpointRef,
        tool_name: &str,
        _timeout: Duration,
    ) -> Result<ToolSchemaResult> {
        self.check_target(endpoint, tool_name)?;
        let descriptor = self
            .connection
            .describe_tool(tool_name)
            .await
            .map_err(|err| BamlRtError::InvalidArgument(err.to_string()))?;
        let digest = compute_descriptor_schema_digest(&descriptor).to_string();
        Ok(ToolSchemaResult {
            schema_version: 2,
            tool_name: descriptor.name,
            content_type: "application/schema+json".to_string(),
            content_digest: digest,
            input: descriptor.input_schema,
            output: descriptor.output_schema,
            events: Vec::new(),
        })
    }

    async fn invoke(
        &self,
        endpoint: &ServiceEndpointRef,
        tool_name: &str,
        input: Value,
        _timeout: Duration,
    ) -> Result<InvokeResponse> {
        self.check_target(endpoint, tool_name)?;
        let result = self
            .connection
            .call_tool(tool_name, input)
            .await
            .map_err(|err| BamlRtError::InvalidArgument(err.to_string()))?;
        let output = serde_json::to_value(result).map_err(|err| {
            BamlRtError::InvalidArgument(format!("MCP result serialization failed: {err}"))
        })?;
        Ok(InvokeResponse { output, done: true })
    }
}
