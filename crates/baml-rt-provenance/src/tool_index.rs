//! Tool metadata indexing for FalkorDB.

use crate::error::Result;
use baml_rt_tools::ToolFunctionMetadataExport;
use serde_json;
use text_to_cypher::core::execute_cypher_query;

const TOOL_LABEL: &str = "ToolFunction";

#[derive(Debug, Clone)]
pub struct ToolIndexConfig {
    pub connection: String,
    pub graph: String,
}

impl ToolIndexConfig {
    pub fn new(connection: impl Into<String>, graph: impl Into<String>) -> Self {
        Self {
            connection: connection.into(),
            graph: graph.into(),
        }
    }
}

pub async fn index_tools(
    config: &ToolIndexConfig,
    tools: &[ToolFunctionMetadataExport],
) -> Result<()> {
    ensure_fulltext_index(config).await?;
    for tool in tools {
        upsert_tool(config, tool).await?;
    }
    Ok(())
}

async fn ensure_fulltext_index(config: &ToolIndexConfig) -> Result<()> {
    let query = format!(
        "CALL db.idx.fulltext.createNodeIndex('{}', 'name', 'description', 'tags')",
        TOOL_LABEL
    );
    match execute_cypher_query(&query, &config.graph, &config.connection, false).await {
        Ok(_) => Ok(()),
        Err(err) => {
            let message = err.to_string().to_lowercase();
            if message.contains("already exists") || message.contains("already defined") {
                Ok(())
            } else {
                Err(err.into())
            }
        }
    }
}

async fn upsert_tool(config: &ToolIndexConfig, tool: &ToolFunctionMetadataExport) -> Result<()> {
    let name = tool.name.to_string();
    let description = tool.description.as_str();
    let tags = tool.tags.join(" ");
    let bundle = tool.name.bundle().to_string();
    let input_type = tool.input_type.name.as_str();
    let output_type = tool.output_type.name.as_str();
    let input_schema = tool.input_schema.to_string();
    let output_schema = tool.output_schema.to_string();
    let secret_requirements = serde_json::to_string(&tool.secret_requirements).unwrap_or_default();
    let is_host_tool = tool.origin == baml_rt_tools::ToolOrigin::Host;

    let query = format!(
        "MERGE (t:{label} {{name: \"{name}\"}})\n\
         SET t.description = \"{description}\",\n\
             t.tags = \"{tags}\",\n\
             t.bundle = \"{bundle}\",\n\
             t.input_type = \"{input_type}\",\n\
             t.output_type = \"{output_type}\",\n\
             t.input_schema = \"{input_schema}\",\n\
             t.output_schema = \"{output_schema}\",\n\
             t.secret_requirements = \"{secret_requirements}\",\n\
             t.is_host_tool = {is_host_tool}",
        label = TOOL_LABEL,
        name = escape_cypher(&name),
        description = escape_cypher(description),
        tags = escape_cypher(&tags),
        bundle = escape_cypher(&bundle),
        input_type = escape_cypher(input_type),
        output_type = escape_cypher(output_type),
        input_schema = escape_cypher(&input_schema),
        output_schema = escape_cypher(&output_schema),
        secret_requirements = escape_cypher(&secret_requirements),
        is_host_tool = if is_host_tool { "true" } else { "false" }
    );

    execute_cypher_query(&query, &config.graph, &config.connection, false)
        .await
        .map(|_| ())
        .map_err(Into::into)
}

fn escape_cypher(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}
