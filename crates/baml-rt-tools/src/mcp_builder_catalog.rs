//! Build-time catalog source that projects approved MCP snapshots into
//! [`ToolFunctionMetadata`].
//!
//! The cache layout produced by the importer (`mcp_cache`) is read here:
//! `<root>/servers/<id>/server.json` plus `<root>/tools/<slug>/tool-metadata.json`.
//! Only servers and tools whose approval state is `Approved` are projected.
//! Pending, rejected, and stale entries are skipped so a builder cannot
//! silently emit a contract against unapproved schemas.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use baml_rt_core::{BamlRtError, Result};
use serde_json::Value;

use crate::{
    ToolName,
    mcp_cache::{ToolRecord, read_server},
    mcp_snapshot::McpOutputMode,
    opaque_json::{OPAQUE_JSON_BAML_TYPE, OPAQUE_JSON_SCHEMA_MARKER_KEY},
    tool_catalog::ToolCatalog,
    tools::{
        BundleName, SecretRequest, SessionPolicy, ToolBackend, ToolFunctionMetadata, ToolOrigin,
        ToolTypeSpec,
    },
};

/// Env var that points the builder at a single MCP snapshot cache root.
/// When unset and no explicit root is supplied, the catalog is empty.
pub const BUILDER_MCP_CACHE_ENV: &str = "BAML_MCP_CACHE_DIR";

/// Read-only catalog backed by an MCP snapshot cache directory.
#[derive(Debug, Default)]
pub struct McpSnapshotCatalog {
    tools: Vec<ToolFunctionMetadata>,
}

impl McpSnapshotCatalog {
    /// Scan a cache root and project every approved tool whose owning server
    /// is also approved.
    pub fn from_root(root: &Path) -> Result<Self> {
        let tools_dir = root.join("tools");
        let entries = match fs::read_dir(&tools_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok(Self { tools: Vec::new() });
            }
            Err(err) => return Err(io_err(&tools_dir, err)),
        };

        let mut tools = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| io_err(&tools_dir, err))?;
            let path = entry.path().join("tool-metadata.json");
            if !path.is_file() {
                continue;
            }
            let raw = fs::read_to_string(&path).map_err(|err| io_err(&path, err))?;
            let record: ToolRecord = serde_json::from_str(&raw).map_err(|err| {
                BamlRtError::InvalidArgumentWithSource {
                    message: format!("failed to parse {}", path.display()),
                    source: Box::new(err),
                }
            })?;
            if !record.tool.approval.state.is_approved() {
                continue;
            }
            let server = read_server(root, &record.server_id).map_err(|err| {
                BamlRtError::InvalidArgumentWithSource {
                    message: format!(
                        "tool {} references missing server `{}` under {}",
                        record.tool.platform_tool_name,
                        record.server_id,
                        root.display()
                    ),
                    source: Box::new(err),
                }
            })?;
            if !server.approval.state.is_approved() {
                continue;
            }
            tools.push(project_tool(&server.server_id, record)?);
        }
        tools.sort_by_key(|a| a.name.to_string());
        Ok(Self { tools })
    }

    /// Convenience: scan `$BAML_MCP_CACHE_DIR`. Returns `None` when the env
    /// var is unset or empty.
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var(BUILDER_MCP_CACHE_ENV) {
            Ok(value) if !value.trim().is_empty() => {
                Self::from_root(&PathBuf::from(value)).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

impl ToolCatalog for McpSnapshotCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> {
        self.tools.iter().find(|t| &t.name == name)
    }

    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
        Box::new(self.tools.iter())
    }
}

/// Project a per-tool cache record into platform `ToolFunctionMetadata`.
/// Exposed so the runtime resolver (PR 4) builds handlers with the same
/// metadata the builder used at codegen time.
pub fn project_tool(server_id: &str, record: ToolRecord) -> Result<ToolFunctionMetadata> {
    let tool_name = ToolName::parse(&record.tool.platform_tool_name)?;
    let class_name = ToolFunctionMetadata::derive_class_name(tool_name.bundle(), tool_name.local());

    let input_schema = match record.tool.opaque_fallback_reason.as_deref() {
        Some(_) => opaque_json_schema(),
        None => record.tool.input_schema.clone(),
    };
    let output_schema = match &record.tool.output_mode {
        McpOutputMode::ContentEnvelope | McpOutputMode::OpaqueJson => opaque_json_schema(),
        McpOutputMode::JsonSchema { schema, .. } => schema.clone(),
    };

    let server_bundle = BundleName::new(format!("mcp_{server_id}"))?;
    // Runtime secret resolution flows through the importer/runner secret
    // chain, not through codegen-time prompts, so the metadata carries an
    // empty list at build time.
    let secret_requests: Vec<SecretRequest> = Vec::new();

    let tags = vec!["mcp".to_string(), format!("mcp:{server_id}")];

    Ok(ToolFunctionMetadata {
        name: tool_name,
        class_name: class_name.clone(),
        description: record
            .tool
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool from server `{server_id}`")),
        open_input_schema: serde_json::json!({}),
        input_schema,
        output_schema,
        open_input_type: ToolTypeSpec {
            name: "()".to_string(),
            ts_decl: None,
        },
        input_type: ToolTypeSpec {
            name: format!("{class_name}Input"),
            ts_decl: None,
        },
        output_type: ToolTypeSpec {
            name: format!("{class_name}Output"),
            ts_decl: None,
        },
        baml_decl: None,
        extra_ts_decls: Vec::new(),
        access: Some(record.tool.access_level),
        tags,
        secret_requests,
        config: None,
        config_bundle: Some(server_bundle),
        origin: ToolOrigin::Host,
        backend: ToolBackend::Mcp,
        digest: Some(record.tool.input_schema_digest.to_string()),
        projection_semantics: None,
        session_policy: SessionPolicy::Strict,
        event_sources: Vec::new(),
        coordination_baml: None,
    })
}

fn opaque_json_schema() -> Value {
    serde_json::json!({
        OPAQUE_JSON_SCHEMA_MARKER_KEY: OPAQUE_JSON_BAML_TYPE,
        "description": "MCP-projected payload; runtime returns the parsed JSON value."
    })
}

fn io_err(path: &Path, err: std::io::Error) -> BamlRtError {
    BamlRtError::InvalidArgumentWithSource {
        message: format!("failed to access MCP cache entry {}", path.display()),
        source: Box::new(err),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        mcp_cache::write_snapshot,
        mcp_snapshot::{
            ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
            McpOutputMode, McpServerSnapshot, McpTransportRef, SecretRef,
        },
        tools::ToolAccess,
    };

    fn approved_tool(name: &str, fallback: Option<&str>) -> McpImportedTool {
        McpImportedTool {
            platform_tool_name: format!("mcp/grafana/{name}"),
            mcp_tool_name: name.into(),
            description: Some(format!("{name} description")),
            input_schema: json!({
                "type": "object",
                "properties": { "q": { "type": "string" } },
                "required": ["q"]
            }),
            input_schema_digest: Digest::new("sha256:input"),
            output_mode: McpOutputMode::ContentEnvelope,
            access_level: ToolAccess::Read,
            approval: ApprovalRecord {
                state: McpApprovalState::Approved,
                owner: Some("op@example.com".into()),
                reviewed_at: Some("epoch:1".into()),
                expires_at: None,
            },
            opaque_fallback_reason: fallback.map(str::to_string),
            annotations: Value::Null,
        }
    }

    fn approved_snapshot(tools: Vec<McpImportedTool>) -> McpServerSnapshot {
        McpServerSnapshot {
            schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
            server_id: "grafana".into(),
            transport: McpTransportRef::Stdio {
                command_ref: "fake".into(),
                args: vec![],
            },
            protocol_version: "2025-06-18".into(),
            server_info: None,
            server_config_digest: Digest::new("sha256:server"),
            server_identity_digest: Digest::new("sha256:identity"),
            tools_digest: Digest::new("sha256:tools"),
            secret_refs: vec![SecretRef::stdio_env("GRAFANA_TOKEN")],
            approval: ApprovalRecord {
                state: McpApprovalState::Approved,
                owner: Some("op@example.com".into()),
                reviewed_at: Some("epoch:1".into()),
                expires_at: None,
            },
            sandbox_profile: Some("mcp-import-restricted-tier1".into()),
            tools,
        }
    }

    #[test]
    fn projects_approved_tools_into_metadata() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(
            dir.path(),
            &approved_snapshot(vec![
                approved_tool("search_dashboards", None),
                approved_tool("list_alerts", None),
            ]),
        )
        .unwrap();

        let catalog = McpSnapshotCatalog::from_root(dir.path()).unwrap();
        assert_eq!(catalog.len(), 2);
        let names: Vec<String> = catalog.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "mcp/grafana/list_alerts".to_string(),
                "mcp/grafana/search_dashboards".to_string(),
            ]
        );

        let parsed = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
        let meta = catalog.by_name(&parsed).unwrap();
        assert_eq!(meta.class_name, "McpGrafanaSearchDashboards");
        assert_eq!(meta.backend, ToolBackend::Mcp);
        assert!(meta.tags.iter().any(|t| t == "mcp"));
        assert!(meta.tags.iter().any(|t| t == "mcp:grafana"));
        // Default content envelope output projects as OpaqueJson schema.
        assert_eq!(
            meta.output_schema
                .get(OPAQUE_JSON_SCHEMA_MARKER_KEY)
                .and_then(Value::as_str),
            Some(OPAQUE_JSON_BAML_TYPE)
        );
    }

    #[test]
    fn opaque_fallback_input_is_projected_as_opaque_json_schema() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(
            dir.path(),
            &approved_snapshot(vec![approved_tool(
                "search_dashboards",
                Some("unsupported `$ref`"),
            )]),
        )
        .unwrap();

        let catalog = McpSnapshotCatalog::from_root(dir.path()).unwrap();
        let parsed = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
        let meta = catalog.by_name(&parsed).unwrap();
        assert_eq!(
            meta.input_schema
                .get(OPAQUE_JSON_SCHEMA_MARKER_KEY)
                .and_then(Value::as_str),
            Some(OPAQUE_JSON_BAML_TYPE)
        );
    }

    #[test]
    fn pending_tool_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut snap = approved_snapshot(vec![approved_tool("search_dashboards", None)]);
        snap.tools[0].approval.state = McpApprovalState::Pending;
        write_snapshot(dir.path(), &snap).unwrap();
        let catalog = McpSnapshotCatalog::from_root(dir.path()).unwrap();
        assert_eq!(catalog.len(), 0);
    }

    #[test]
    fn pending_server_skips_all_tools_even_if_tool_is_approved() {
        let dir = tempfile::tempdir().unwrap();
        let mut snap = approved_snapshot(vec![approved_tool("search_dashboards", None)]);
        snap.approval.state = McpApprovalState::Pending;
        write_snapshot(dir.path(), &snap).unwrap();
        let catalog = McpSnapshotCatalog::from_root(dir.path()).unwrap();
        assert_eq!(catalog.len(), 0);
    }

    #[test]
    fn rejected_and_stale_states_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut snap = approved_snapshot(vec![
            approved_tool("search_dashboards", None),
            approved_tool("list_alerts", None),
        ]);
        snap.tools[0].approval.state = McpApprovalState::Rejected;
        snap.tools[1].approval.state = McpApprovalState::Stale;
        write_snapshot(dir.path(), &snap).unwrap();
        let catalog = McpSnapshotCatalog::from_root(dir.path()).unwrap();
        assert_eq!(catalog.len(), 0);
    }

    #[test]
    fn empty_cache_root_returns_empty_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = McpSnapshotCatalog::from_root(dir.path()).unwrap();
        assert!(catalog.is_empty());
    }
}
