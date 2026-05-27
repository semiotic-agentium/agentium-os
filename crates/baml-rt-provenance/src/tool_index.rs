//! Tool metadata indexing via the SurrealDB provenance store.
//!
//! Indexes `ToolFunction` records into `prov_node` so they can be queried for
//! discovery, capability matching, and schema lookup.

use baml_rt_tools::ToolFunctionMetadataExport;
use serde_json::Value;

use crate::{error::Result, surreal_store::SurrealProvenanceStore, surreal_write_batch::TxBind};

const TOOL_LABEL: &str = "ToolFunction";

const UPSERT_TOOL_SQL: &str = "UPSERT prov_node SET \
    node_id = $node_id, \
    label = $label, \
    props.description = $description, \
    props.tags = $tags, \
    props.bundle = $bundle, \
    props.input_type = $input_type, \
    props.output_type = $output_type, \
    props.input_schema = $input_schema, \
    props.output_schema = $output_schema, \
    props.secret_requirements = $secret_requirements, \
    props.is_host_tool = $is_host_tool \
    WHERE node_id = $node_id";

#[derive(Debug, Clone)]
pub struct ToolIndexConfig {
    pub path: String,
}

impl ToolIndexConfig {
    pub fn new(path: impl AsRef<std::path::Path>) -> Self {
        Self {
            path: path
                .as_ref()
                .to_str()
                .map(str::to_string)
                .unwrap_or_else(|| path.as_ref().to_string_lossy().into_owned()),
        }
    }

    pub fn in_memory() -> Self {
        Self {
            path: ":memory:".to_string(),
        }
    }
}

/// Index tools into an existing SurrealDB provenance store.
pub async fn index_tools(
    store: &SurrealProvenanceStore,
    tools: &[ToolFunctionMetadataExport],
) -> Result<()> {
    for tool in tools {
        upsert_tool(store, tool).await?;
    }
    Ok(())
}

async fn upsert_tool(
    store: &SurrealProvenanceStore,
    tool: &ToolFunctionMetadataExport,
) -> Result<()> {
    let name = tool.name.to_string();
    let description = tool.description.to_string();
    let tags = tool.tags.join(" ");
    let bundle = tool.name.bundle().to_string();
    let input_type = tool.input_type.name.clone();
    let output_type = tool.output_type.name.clone();
    let input_schema = tool.input_schema.to_string();
    let output_schema = tool.output_schema.to_string();
    let secret_requirements = serde_json::to_string(&tool.secret_requests).unwrap_or_default();
    let is_host_tool = tool.origin == baml_rt_tools::ToolOrigin::Host;

    // Individual prop fields (same pattern as surreal_store's write_normalized)
    // rather than binding a JSON object to `props`, which SurrealDB's bind API
    // does not reliably persist on schemaless tables.
    //
    // The UPSERT is keyed on `node_id` and is idempotent, so it shares the
    // store's MVCC retry budget: in the multi-pod shared-SurrealDB topology
    // every runner indexes the same tool rows concurrently and races on the
    // same `prov_node` records, and the loser of each conflict must retry
    // rather than surface `Transaction conflict` to the best-effort caller.
    let binds = vec![
        TxBind {
            name: "node_id".to_string(),
            value: Value::String(name),
        },
        TxBind {
            name: "label".to_string(),
            value: Value::String(TOOL_LABEL.to_string()),
        },
        TxBind {
            name: "description".to_string(),
            value: Value::String(description),
        },
        TxBind {
            name: "tags".to_string(),
            value: Value::String(tags),
        },
        TxBind {
            name: "bundle".to_string(),
            value: Value::String(bundle),
        },
        TxBind {
            name: "input_type".to_string(),
            value: Value::String(input_type),
        },
        TxBind {
            name: "output_type".to_string(),
            value: Value::String(output_type),
        },
        TxBind {
            name: "input_schema".to_string(),
            value: Value::String(input_schema),
        },
        TxBind {
            name: "output_schema".to_string(),
            value: Value::String(output_schema),
        },
        TxBind {
            name: "secret_requirements".to_string(),
            value: Value::String(secret_requirements),
        },
        TxBind {
            name: "is_host_tool".to_string(),
            value: Value::Bool(is_host_tool),
        },
    ];

    store.run_write_with_retry(UPSERT_TOOL_SQL, &binds).await
}
