// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Orchestrator for the runtime codegen pipeline. The real work lives in
//! [`super::codegen_pipeline`]; this file contains only the [`TypeGenerator`] impl that wires
//! the named phases together and bridges between blocking and async halves.

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use baml_rt_repository::RepositoryService;
use baml_rt_tools::{StaticToolSnapshotCatalog, ToolName, tool_catalog::ToolCatalog};
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
    registry_service: Option<Arc<RepositoryService>>,
    /// Single repository registry that serves both MCP and external-tool
    /// approved snapshots (one runner, one URL). Only [`new`](Self::new) reads it
    /// from the `BAML_REGISTRY_URL` env var; explicit constructors set it directly.
    registry_url: Option<String>,
    snapshot_cache_root: Option<PathBuf>,
}

impl RuntimeTypeGenerator {
    pub fn new() -> Self {
        Self {
            registry_service: None,
            registry_url: std::env::var("BAML_REGISTRY_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            snapshot_cache_root: None,
        }
    }

    pub fn with_registry_service(service: Arc<RepositoryService>) -> Self {
        Self {
            registry_service: Some(service),
            registry_url: None,
            snapshot_cache_root: None,
        }
    }

    pub fn with_registry_url(url: impl Into<String>) -> Self {
        Self {
            registry_service: None,
            registry_url: Some(url.into()),
            snapshot_cache_root: None,
        }
    }

    pub fn with_snapshot_cache(root: impl Into<PathBuf>) -> Self {
        Self {
            registry_service: None,
            registry_url: None,
            snapshot_cache_root: Some(root.into()),
        }
    }
}

#[async_trait::async_trait]
impl TypeGenerator for RuntimeTypeGenerator {
    async fn generate(&self, agent_dir: &AgentDir, build_dir: &BuildDir) -> Result<()> {
        let static_catalog = prepare_static_tool_catalog(self).await?;
        prepare_mcp_snapshots(agent_dir, build_dir, self).await?;
        prepare_external_tool_snapshots(agent_dir, build_dir, self, static_catalog.as_ref())
            .await?;

        let agent_dir = agent_dir.clone();
        let build_dir = build_dir.clone();

        // Sync half: workspace materialise, BAML compiles (with the universal authored-prompt
        // rewriter in between), session artifact generation, stable catalog planning, manifest
        // + .d.ts emission.
        let catalog_render_inputs = task::spawn_blocking(move || {
            let workspace = WorkspaceReady::materialize_with_static_catalog(
                agent_dir,
                build_dir,
                static_catalog,
            )?;
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

async fn prepare_static_tool_catalog(
    generator: &RuntimeTypeGenerator,
) -> Result<Option<StaticToolSnapshotCatalog>> {
    if let Some(service) = &generator.registry_service {
        let response = service.static_tool_catalog().cloned().ok_or_else(|| {
            BamlBuilderError::InvalidArgument(
                "static tool catalog not available from embedded repository service; host runner did not inject its inventory"
                    .to_string(),
            )
        })?;
        let catalog = StaticToolSnapshotCatalog::from_response(response)?;
        tracing::info!(
            static_tools = catalog.len(),
            static_tool_registry_source = "embedded",
            "resolved static tool catalog for agent build"
        );
        return Ok(Some(catalog));
    }

    if let Some(registry_url) = &generator.registry_url {
        let catalog = crate::static_tool_registry::fetch_static_tool_catalog(registry_url)
            .await
            .map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
                message: format!(
                    "failed to fetch static tool catalog from registry {registry_url}. Run a compatible runner, pass --static-tool-catalog for offline builds, or pass --use-local-inventory for dev-only local inventory."
                ),
                source: err.into(),
            })?;
        tracing::info!(
            static_tools = catalog.len(),
            static_tool_registry_source = %registry_url,
            "resolved static tool catalog for agent build"
        );
        return Ok(Some(catalog));
    }

    Ok(None)
}

async fn prepare_external_tool_snapshots(
    agent_dir: &AgentDir,
    build_dir: &BuildDir,
    generator: &RuntimeTypeGenerator,
    static_catalog: Option<&StaticToolSnapshotCatalog>,
) -> Result<()> {
    let required_external_tools = manifest_external_tool_names(agent_dir, static_catalog)?;
    if required_external_tools.is_empty() {
        return Ok(());
    }

    let root = build_dir.as_path();
    let mut resolved = HashSet::new();
    if let Some(service) = &generator.registry_service {
        let snapshots = service
            .list_approved_external_tool_snapshots()
            .await
            .map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
                message: "failed to fetch external-tool snapshots from registry".to_string(),
                source: Box::new(err),
            })?;
        for snapshot in snapshots
            .into_iter()
            .filter(|snapshot| required_external_tools.contains(&snapshot.tool.name))
        {
            resolved.insert(snapshot.tool.name.clone());
            tracing::info!(
                external_tool = %snapshot.tool.name,
                external_tool_schema_digest = %snapshot.digests.schema_digest,
                external_tool_runtime_digest = %snapshot.digests.runtime_digest,
                external_tool_registry_source = "embedded",
                "resolved manifest-referenced external-tool snapshot for agent build"
            );
            baml_rt_tools::external_tool_cache::write_approved_snapshot(root, &snapshot)
                .map_err(BamlBuilderError::Io)?;
        }
        ensure_external_snapshots_resolved(&required_external_tools, &resolved)?;
        return Ok(());
    }

    if let Some(snapshot_cache_root) = &generator.snapshot_cache_root {
        let cache_root =
            baml_rt_tools::external_tool_cache::resolve_cache_root(snapshot_cache_root);
        let snapshots = baml_rt_tools::external_tool_cache::read_approved_snapshots(&cache_root)
            .map_err(BamlBuilderError::from)?;
        for snapshot in snapshots
            .into_iter()
            .filter(|snapshot| required_external_tools.contains(&snapshot.tool.name))
        {
            resolved.insert(snapshot.tool.name.clone());
            tracing::info!(
                external_tool = %snapshot.tool.name,
                external_tool_schema_digest = %snapshot.digests.schema_digest,
                external_tool_runtime_digest = %snapshot.digests.runtime_digest,
                external_tool_registry_source = %cache_root.display(),
                "resolved manifest-referenced external-tool snapshot from explicit snapshot cache"
            );
            baml_rt_tools::external_tool_cache::write_approved_snapshot(root, &snapshot)
                .map_err(BamlBuilderError::Io)?;
        }
        ensure_external_snapshots_resolved(&required_external_tools, &resolved)?;
        return Ok(());
    }

    let Some(registry_url) = &generator.registry_url else {
        ensure_external_snapshots_resolved(&required_external_tools, &resolved)?;
        return Ok(());
    };
    let url = format!(
        "{}/external-tools/snapshots",
        registry_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
            message: format!("failed to fetch external-tool snapshots from {url}"),
            source: Box::new(err),
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(BamlBuilderError::InvalidArgument(format!(
            "failed to fetch external-tool snapshots from registry ({status}): {body}"
        )));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
            message: "failed to parse external-tool snapshots from registry".to_string(),
            source: Box::new(err),
        })?;
    let snapshots: Vec<baml_rt_tools::external_tools::ExternalToolSnapshot> =
        serde_json::from_value(
            parsed
                .get("snapshots")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
        )
        .map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
            message: "failed to decode external-tool snapshot list from registry".to_string(),
            source: Box::new(err),
        })?;
    for snapshot in snapshots
        .into_iter()
        .filter(|snapshot| required_external_tools.contains(&snapshot.tool.name))
    {
        resolved.insert(snapshot.tool.name.clone());
        tracing::info!(
            external_tool = %snapshot.tool.name,
            external_tool_schema_digest = %snapshot.digests.schema_digest,
            external_tool_runtime_digest = %snapshot.digests.runtime_digest,
            external_tool_registry_source = %registry_url,
            "resolved manifest-referenced external-tool snapshot for agent build"
        );
        baml_rt_tools::external_tool_cache::write_approved_snapshot(root, &snapshot)
            .map_err(BamlBuilderError::Io)?;
    }
    ensure_external_snapshots_resolved(&required_external_tools, &resolved)
}

fn manifest_external_tool_names(
    agent_dir: &AgentDir,
    static_catalog: Option<&StaticToolSnapshotCatalog>,
) -> Result<HashSet<String>> {
    let tool_names = load_manifest_tools(&agent_dir.baml_src())?;
    if tool_names.is_empty() {
        return Ok(HashSet::new());
    }

    let static_tool_names: HashSet<String> = match static_catalog {
        Some(catalog) => catalog.iter().map(|tool| tool.name.to_string()).collect(),
        None => {
            let inventory = baml_rt_tools::tool_catalog::InventoryCatalog::new();
            inventory.iter().map(|tool| tool.name.to_string()).collect()
        }
    };

    let mut external_tools = HashSet::new();
    for name in tool_names {
        if name.starts_with("mcp/") || static_tool_names.contains(&name) {
            continue;
        }
        ToolName::parse(&name).map_err(BamlBuilderError::from)?;
        external_tools.insert(name);
    }
    Ok(external_tools)
}

fn ensure_external_snapshots_resolved(
    required: &HashSet<String>,
    resolved: &HashSet<String>,
) -> Result<()> {
    let mut missing: Vec<&String> = required
        .iter()
        .filter(|name| !resolved.contains(*name))
        .collect();
    missing.sort();
    if let Some(name) = missing.first() {
        return Err(BamlBuilderError::InvalidArgument(format!(
            "manifest uses external tool {name}, but no approved registry snapshot was found"
        )));
    }
    Ok(())
}

async fn prepare_mcp_snapshots(
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
    if let Some(service) = &generator.registry_service {
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
                        "manifest uses MCP server {server_id}, but no approved registry snapshot was found"
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

    if let Some(snapshot_cache_root) = &generator.snapshot_cache_root {
        let cache_root = baml_rt_tools::mcp_cache::resolve_cache_root(snapshot_cache_root);
        for server_id in &server_ids {
            let snapshot = baml_rt_tools::mcp_cache::read_snapshot(&cache_root, server_id).map_err(
                |_| {
                    BamlBuilderError::InvalidArgument(format!(
                        "manifest uses MCP server {server_id}, but no approved registry snapshot was found"
                    ))
                },
            )?;
            if !snapshot.approval.state.is_approved() {
                return Err(BamlBuilderError::InvalidArgument(format!(
                    "manifest uses MCP server {server_id}, but no approved registry snapshot was found"
                )));
            }
            tracing::info!(
                mcp_server_id = %server_id,
                mcp_tools = snapshot.tools.len(),
                mcp_tools_digest = %snapshot.tools_digest,
                mcp_registry_source = %cache_root.display(),
                "resolved MCP snapshot from explicit snapshot cache for agent build"
            );
            baml_rt_tools::mcp_cache::write_snapshot(&root, &snapshot)
                .map_err(BamlBuilderError::Io)?;
        }
        return Ok(());
    }

    let Some(registry_url) = &generator.registry_url else {
        return Err(BamlBuilderError::InvalidArgument(format!(
            "manifest uses MCP server {}, but no approved registry snapshot was found",
            server_ids[0]
        )));
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
                "manifest uses MCP server {server_id}, but no approved registry snapshot was found"
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
