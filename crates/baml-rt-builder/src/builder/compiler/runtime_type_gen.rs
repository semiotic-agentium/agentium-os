// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Orchestrator for the runtime codegen pipeline. The real work lives in
//! [`super::codegen_pipeline`]; this file contains only the [`TypeGenerator`] impl that wires
//! the named phases together and bridges between blocking and async halves.

use std::sync::Arc;

use baml_rt_repository::RepositoryService;
use tokio::task;

use super::codegen_pipeline::WorkspaceReady;
use crate::builder::{
    error::{BamlBuilderError, Result},
    traits::TypeGenerator,
    ts_gen::load_manifest_tools,
    types::{AgentDir, BuildDir},
};

/// Type generator for runtime declarations.
///
/// Writes `baml-runtime.d.ts` into the agent's `src/` directory so that both `tsc` and IDEs
/// can resolve the types without any temp-dir indirection. All other artifacts (generated BAML
/// prelude, session-plan / unified-primary / tool-step-executor manifests, external-tools
/// lockfile, and the rendered tool-schema catalog sidecar) land under `build_dir/`.
#[derive(Clone, Default)]
pub struct RuntimeTypeGenerator {
    mcp_registry_service: Option<Arc<RepositoryService>>,
    mcp_registry_url: Option<String>,
}

impl RuntimeTypeGenerator {
    pub fn new() -> Self {
        Self {
            mcp_registry_service: None,
            mcp_registry_url: std::env::var("BAML_MCP_REGISTRY_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
        }
    }

    pub fn with_mcp_registry_service(service: Arc<RepositoryService>) -> Self {
        Self {
            mcp_registry_service: Some(service),
            mcp_registry_url: None,
        }
    }

    pub fn with_mcp_registry_url(url: impl Into<String>) -> Self {
        Self {
            mcp_registry_service: None,
            mcp_registry_url: Some(url.into()),
        }
    }
}

#[async_trait::async_trait]
impl TypeGenerator for RuntimeTypeGenerator {
    async fn generate(&self, agent_dir: &AgentDir, build_dir: &BuildDir) -> Result<()> {
        prepare_mcp_registry_cache(agent_dir, build_dir, self).await?;

        let agent_dir = agent_dir.clone();
        let build_dir = build_dir.clone();

        // Sync half: workspace materialise, BAML compiles (with the universal authored-prompt
        // rewriter in between), session artifact generation, stable catalog planning, manifest
        // + .d.ts emission.
        let catalog_render_inputs = task::spawn_blocking(move || {
            let workspace = WorkspaceReady::materialize(agent_dir, build_dir)?;
            let prelude = workspace.emit_tool_interfaces_prelude()?;
            let compiled = prelude.compile_first_pass()?;
            let normalized = compiled.normalize_authored_prompts()?;
            let session_emitted = normalized.emit_session_artifacts()?;
            let runtime_finalized = session_emitted.append_catalog_function_and_finalize()?;
            runtime_finalized.emit_typescript_declarations()?;
            runtime_finalized.emit_runtime_manifests()?;
            Ok::<_, BamlBuilderError>(runtime_finalized.into_catalog_render_inputs())
        })
        .await
        .map_err(|e| BamlBuilderError::BlockingTaskJoin { source: e })??;

        // Async tail: persist the IR-derived stable tool / operation vocabulary sidecar that the
        // runtime loads into `ctx.tags['tool_schema_prelude']` at agent load time.
        if let Some(inputs) = catalog_render_inputs {
            inputs.render_sidecar().await?;
        }
        Ok(())
    }
}

async fn prepare_mcp_registry_cache(
    agent_dir: &AgentDir,
    build_dir: &BuildDir,
    generator: &RuntimeTypeGenerator,
) -> Result<()> {
    let tool_names = load_manifest_tools(&agent_dir.baml_src())?;
    let mut server_ids: Vec<String> = tool_names
        .iter()
        .filter_map(|name| {
            let rest = name.strip_prefix("mcp/")?;
            rest.split('/').next().map(str::to_string)
        })
        .collect();
    server_ids.sort();
    server_ids.dedup();
    if server_ids.is_empty() {
        return Ok(());
    }

    let root = build_dir.join("mcp");
    if let Some(service) = &generator.mcp_registry_service {
        for server_id in &server_ids {
            let snapshot = service
                .get_latest_mcp_snapshot(server_id)
                .await
                .map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
                    message: format!("failed to fetch MCP snapshot `{server_id}` from registry"),
                    source: Box::new(err),
                })?
                .ok_or_else(|| {
                    BamlBuilderError::InvalidArgument(format!(
                        "manifest uses MCP server `{server_id}`, but no approved registry snapshot was found; if the latest snapshot is stale, re-import and approve a new version"
                    ))
                })?;
            tracing::info!(
                mcp_server_id = %server_id,
                mcp_tools = snapshot.tools.len(),
                mcp_tools_digest = %snapshot.tools_digest,
                mcp_registry_source = "embedded",
                "resolved MCP snapshot for agent build"
            );
            baml_rt_tools::mcp_cache::write_snapshot(&root, &snapshot)
                .map_err(BamlBuilderError::Io)?;
        }
        return Ok(());
    }

    let Some(registry_url) = &generator.mcp_registry_url else {
        return Ok(());
    };
    let http = reqwest::Client::new();
    for server_id in &server_ids {
        let url = format!(
            "{}/mcp/servers/{server_id}",
            registry_url.trim_end_matches('/')
        );
        let response = http.get(&url).send().await.map_err(|err| {
            BamlBuilderError::InvalidArgumentWithSource {
                message: format!("failed to fetch MCP snapshot `{server_id}` from {url}"),
                source: Box::new(err),
            }
        })?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(BamlBuilderError::InvalidArgument(format!(
                "failed to fetch MCP snapshot `{server_id}` from registry ({status}): {body}"
            )));
        }
        let snapshot: baml_rt_tools::mcp_snapshot::McpServerSnapshot = serde_json::from_str(&body)
            .map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
                message: format!("failed to parse MCP snapshot `{server_id}` from registry"),
                source: Box::new(err),
            })?;
        tracing::info!(
            mcp_server_id = %server_id,
            mcp_tools = snapshot.tools.len(),
            mcp_tools_digest = %snapshot.tools_digest,
            mcp_registry_source = %registry_url,
            "resolved MCP snapshot for agent build"
        );
        baml_rt_tools::mcp_cache::write_snapshot(&root, &snapshot).map_err(BamlBuilderError::Io)?;
    }
    Ok(())
}
