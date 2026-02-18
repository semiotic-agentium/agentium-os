//! Tool metadata indexing for GraphQLite-backed provenance store.
//!
//! Node identity uses GraphQLite's built-in `id` property (see [vocabulary::graph::NODE_ID])
//! so the extension can index and match on identity.
//!
//! **GraphQLite MERGE and parameters:** The extension's MERGE executor only applies
//! pattern properties that are AST literals; it does not resolve `$param` from the
//! params JSON. So we put the identity value in the MERGE pattern as an escaped literal.
//! Remaining properties are set via a separate MATCH+SET query, which uses the
//! transform path and param binding. See upstream executor_merge.c / find_node_by_pattern.

use crate::error::Result;
use crate::vocabulary::graph;
use baml_rt_tools::ToolFunctionMetadataExport;
use graphqlite::{Connection, escape_string};
use serde_json::json;
use std::path::Path;

const TOOL_LABEL: &str = "ToolFunction";

#[derive(Debug, Clone)]
pub struct ToolIndexConfig {
    /// Path to the GraphQLite database file (or ":memory:" for in-memory).
    pub path: String,
}

impl ToolIndexConfig {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path
                .as_ref()
                .to_str()
                .map(str::to_string)
                .unwrap_or_else(|| path.as_ref().to_string_lossy().into_owned()),
        }
    }

    /// In-memory store for tests.
    pub fn in_memory() -> Self {
        Self {
            path: ":memory:".to_string(),
        }
    }
}

pub async fn index_tools(
    config: &ToolIndexConfig,
    tools: &[ToolFunctionMetadataExport],
) -> Result<()> {
    let conn = Connection::open(&config.path)
        .map_err(|e| crate::error::ProvenanceError::Storage(Box::new(e)))?;
    for tool in tools {
        upsert_tool_sync(&conn, tool)?;
    }
    Ok(())
}

/// Index tools and return the open connection so the caller can query within the same connection
/// (avoids cross-connection visibility issues in tests).
pub async fn index_tools_into_connection(
    config: &ToolIndexConfig,
    tools: &[ToolFunctionMetadataExport],
) -> Result<Connection> {
    let conn = Connection::open(&config.path)
        .map_err(|e| crate::error::ProvenanceError::Storage(Box::new(e)))?;
    for tool in tools {
        upsert_tool_sync(&conn, tool)?;
    }
    Ok(conn)
}

fn upsert_tool_sync(conn: &Connection, tool: &ToolFunctionMetadataExport) -> Result<()> {
    let name = tool.name.to_string();
    let description = tool.description.to_string();
    let tags = tool.tags.join(" ");
    let bundle = tool.name.bundle().to_string();
    let input_type = tool.input_type.name.clone();
    let output_type = tool.output_type.name.clone();
    let input_schema = tool.input_schema.to_string();
    let output_schema = tool.output_schema.to_string();
    let secret_requirements = serde_json::to_string(&tool.secret_requirements).unwrap_or_default();
    let is_host_tool = tool.origin == baml_rt_tools::ToolOrigin::Host;

    // MERGE with literal id only: extension's MERGE path does not resolve $param in pattern
    // (executor_merge.c uses only AST_NODE_LITERAL). Literal id gives us built-in identity/index.
    let id_literal = escape_string(&name);
    let merge_query = format!(
        "MERGE (t:{label} {{{id_key}: '{id_escaped}'}})",
        label = TOOL_LABEL,
        id_key = graph::NODE_ID,
        id_escaped = id_literal,
    );
    conn.cypher(&merge_query)
        .map_err(|e| crate::error::ProvenanceError::Storage(Box::new(e)))?;

    let set_params = json!({
        "id": name,
        "description": description,
        "tags": tags,
        "bundle": bundle,
        "input_type": input_type,
        "output_type": output_type,
        "input_schema": input_schema,
        "output_schema": output_schema,
        "secret_requirements": secret_requirements,
        "is_host_tool": is_host_tool,
    });
    let set_query = format!(
        "MATCH (t:{label}) WHERE t.{id_key} = $id\n\
         SET t.description = $description,\n\
             t.tags = $tags,\n\
             t.bundle = $bundle,\n\
             t.input_type = $input_type,\n\
             t.output_type = $output_type,\n\
             t.input_schema = $input_schema,\n\
             t.output_schema = $output_schema,\n\
             t.secret_requirements = $secret_requirements,\n\
             t.is_host_tool = $is_host_tool",
        label = TOOL_LABEL,
        id_key = graph::NODE_ID,
    );
    conn.cypher_builder(&set_query)
        .params(&set_params)
        .run()
        .map(|_| ())
        .map_err(|e| crate::error::ProvenanceError::Storage(Box::new(e)))
}
