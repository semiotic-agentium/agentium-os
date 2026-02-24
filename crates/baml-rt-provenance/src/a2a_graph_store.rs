use async_trait::async_trait;
use baml_rt_vocabulary::{
    A2aGraphStore, A2aGraphStoreResult, TaskSubgraphNode, TaskSubgraphUpdateNode,
};
use serde_json::Value;

use crate::{GraphQueryParams, GraphRow, GraphqliteProvenanceStore};

const TASK_NODE_LABEL: &str = "A2ATaskSubgraph";
const TASK_MESSAGE_NODE_LABEL: &str = "A2ATaskMessageSubgraph";
const TASK_UPDATE_NODE_LABEL: &str = "A2ATaskUpdateSubgraph";

fn qp(key: &str, value: impl Into<Value>) -> (String, Value) {
    (key.to_string(), value.into())
}

fn qparams<const N: usize>(pairs: [(String, Value); N]) -> GraphQueryParams {
    let mut params = GraphQueryParams::with_capacity(N);
    for (k, v) in pairs {
        params.insert(k, v);
    }
    params
}

fn empty_params() -> GraphQueryParams {
    GraphQueryParams::new()
}

fn task_id_storage_key(task_id: &str) -> String {
    // Hex-encode UTF-8 bytes so GraphQLite task subgraph lookups don't depend on
    // backend handling of control whitespace in Cypher string literals/params.
    let mut out = String::with_capacity(task_id.len() * 2);
    for byte in task_id.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn escape_cypher_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn decode_task_node(row: &GraphRow) -> Option<TaskSubgraphNode> {
    Some(TaskSubgraphNode {
        id: row.get::<String>("id").ok()?,
        context_id: row.get::<String>("context_id").ok().unwrap_or_default(),
        status_json: row.get::<String>("status_json").ok().unwrap_or_default(),
        metadata_json: row.get::<String>("metadata_json").ok().unwrap_or_default(),
        extra_json: row.get::<String>("extra_json").ok().unwrap_or_default(),
        artifacts_json: row.get::<String>("artifacts_json").ok().unwrap_or_default(),
    })
}

fn decode_update_node(row: &GraphRow) -> Option<TaskSubgraphUpdateNode> {
    Some(TaskSubgraphUpdateNode {
        id: row.get::<String>("id").ok()?,
        kind: row.get::<String>("kind").ok()?,
        payload_json: row.get::<String>("payload_json").ok()?,
    })
}

async fn max_seq_for_label(
    store: &GraphqliteProvenanceStore,
    label: &str,
    task_id: &str,
) -> A2aGraphStoreResult<i64> {
    let task_id_key = task_id_storage_key(task_id);
    let query = format!(
        "MATCH (n:{label}) \
         WHERE n.task_id_key = $task_id_key \
         RETURN coalesce(max(n.seq), 0) AS max_seq"
    );
    let rows = store
        .run_cypher_read(&query, &qparams([qp("task_id_key", task_id_key)]))
        .await
        .map_err(|e| e.to_string())?;
    let max_seq = rows
        .iter()
        .next()
        .and_then(|row| row.get::<i64>("max_seq").ok())
        .unwrap_or(0);
    if max_seq > 0 {
        return Ok(max_seq);
    }

    // Legacy compatibility: older rows may not have task_id_key. Fall back to a
    // broader scan and filter in Rust instead of GraphQLite param comparisons.
    let legacy_rows = store
        .run_cypher_read(
            &format!("MATCH (n:{label}) RETURN n.task_id AS task_id, n.seq AS seq"),
            &empty_params(),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(legacy_rows
        .iter()
        .filter(|row| row.get::<String>("task_id").ok().as_deref() == Some(task_id))
        .filter_map(|row| row.get::<i64>("seq").ok())
        .max()
        .unwrap_or(0))
}

#[async_trait]
impl A2aGraphStore for GraphqliteProvenanceStore {
    async fn max_task_ord(&self) -> A2aGraphStoreResult<i64> {
        let rows = self
            .run_cypher_read(
                &format!("MATCH (n:{TASK_NODE_LABEL}) RETURN coalesce(max(n.ord), 0) AS max_ord"),
                &empty_params(),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .next()
            .and_then(|row| row.get::<i64>("max_ord").ok())
            .unwrap_or(0))
    }

    async fn max_message_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64> {
        max_seq_for_label(self, TASK_MESSAGE_NODE_LABEL, task_id).await
    }

    async fn max_update_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64> {
        max_seq_for_label(self, TASK_UPDATE_NODE_LABEL, task_id).await
    }

    async fn get_task_node(&self, id: &str) -> A2aGraphStoreResult<Option<TaskSubgraphNode>> {
        let id_lit = escape_cypher_string(id);
        let rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (n:{TASK_NODE_LABEL}) WHERE n.id = '{id_lit}' \
                     RETURN n.id AS id, n.context_id AS context_id, n.status_json AS status_json, \
                            n.metadata_json AS metadata_json, n.extra_json AS extra_json, n.artifacts_json AS artifacts_json"
                ),
                &empty_params(),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(rows.iter().next().and_then(decode_task_node))
    }

    async fn list_task_nodes(
        &self,
        context_id: Option<&str>,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphNode>> {
        let (query, params) = if let Some(cid) = context_id {
            (
                format!(
                    "MATCH (n:{TASK_NODE_LABEL}) WHERE n.context_id = $context_id \
                     RETURN n.id AS id, n.context_id AS context_id, n.status_json AS status_json, \
                            n.metadata_json AS metadata_json, n.extra_json AS extra_json, n.artifacts_json AS artifacts_json \
                     ORDER BY n.ord"
                ),
                qparams([qp("context_id", cid)]),
            )
        } else {
            (
                format!(
                    "MATCH (n:{TASK_NODE_LABEL}) \
                     RETURN n.id AS id, n.context_id AS context_id, n.status_json AS status_json, \
                            n.metadata_json AS metadata_json, n.extra_json AS extra_json, n.artifacts_json AS artifacts_json \
                     ORDER BY n.ord"
                ),
                empty_params(),
            )
        };
        let rows = self
            .run_cypher_read(&query, &params)
            .await
            .map_err(|e| e.to_string())?;
        Ok(rows.iter().filter_map(decode_task_node).collect())
    }

    async fn upsert_task_node(
        &self,
        node: &TaskSubgraphNode,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()> {
        // GraphQLite may not bind $id in MERGE patterns; inline the id literal for stable matching.
        let id_lit = escape_cypher_string(&node.id);
        self.run_cypher_execute(
            &format!(
                "MERGE (n:{TASK_NODE_LABEL} {{id: '{id_lit}'}}) \
                 ON CREATE SET n.ord = $ord, \
                     n.context_id = $context_id, \
                     n.status_json = $status_json, \
                     n.metadata_json = $metadata_json, \
                     n.extra_json = $extra_json, \
                     n.artifacts_json = $artifacts_json \
                 ON MATCH SET n.context_id = $context_id, \
                     n.status_json = $status_json, \
                     n.metadata_json = $metadata_json, \
                     n.extra_json = $extra_json, \
                     n.artifacts_json = $artifacts_json"
            ),
            &qparams([
                qp("ord", ord_if_create),
                qp("context_id", node.context_id.clone()),
                qp("status_json", node.status_json.clone()),
                qp("metadata_json", node.metadata_json.clone()),
                qp("extra_json", node.extra_json.clone()),
                qp("artifacts_json", node.artifacts_json.clone()),
            ]),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn ensure_task_node(
        &self,
        id: &str,
        context_id: &str,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()> {
        let id_lit = escape_cypher_string(id);
        self.run_cypher_execute(
            &format!(
                "MERGE (n:{TASK_NODE_LABEL} {{id: '{id_lit}'}}) \
                 ON CREATE SET n.context_id = $context_id, n.status_json = '', n.metadata_json = '{{}}', n.extra_json = '{{}}', n.artifacts_json = '[]', n.ord = $ord"
            ),
            &qparams([qp("context_id", context_id), qp("ord", ord_if_create)]),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn insert_message_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        message_json: &str,
    ) -> A2aGraphStoreResult<()> {
        let id_lit = escape_cypher_string(id);
        let task_id_key = task_id_storage_key(task_id);
        let task_id_lit = escape_cypher_string(task_id);
        let message_json_lit = escape_cypher_string(message_json);
        self.run_cypher_execute(
            &format!(
                "MERGE (n:{TASK_MESSAGE_NODE_LABEL} {{id: '{id_lit}'}}) \
                 ON CREATE SET n.task_id = '{task_id_lit}', n.task_id_key = '{task_id_key}', n.seq = {seq}, n.message_json = '{message_json_lit}' \
                 ON MATCH SET n.task_id = '{task_id_lit}', n.task_id_key = '{task_id_key}', n.seq = {seq}, n.message_json = '{message_json_lit}'"
            ),
            &empty_params(),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn list_message_json(&self, task_id: &str) -> A2aGraphStoreResult<Vec<String>> {
        let task_id_key = task_id_storage_key(task_id);
        let rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (m:{TASK_MESSAGE_NODE_LABEL}) \
                     WHERE m.task_id_key = $task_id_key \
                     RETURN m.message_json AS message_json ORDER BY m.seq"
                ),
                &qparams([qp("task_id_key", task_id_key)]),
            )
            .await
            .map_err(|e| e.to_string())?;
        let out: Vec<String> = rows
            .iter()
            .filter_map(|row| row.get::<String>("message_json").ok())
            .collect();
        if !out.is_empty() {
            return Ok(out);
        }

        let legacy_rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (m:{TASK_MESSAGE_NODE_LABEL}) \
                     RETURN m.task_id AS task_id, m.message_json AS message_json ORDER BY m.seq"
                ),
                &empty_params(),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(legacy_rows
            .iter()
            .filter(|row| row.get::<String>("task_id").ok().as_deref() == Some(task_id))
            .filter_map(|row| row.get::<String>("message_json").ok())
            .collect())
    }

    async fn set_task_status_json(&self, id: &str, status_json: &str) -> A2aGraphStoreResult<()> {
        let id_lit = escape_cypher_string(id);
        self.run_cypher_execute(
            &format!("MATCH (n:{TASK_NODE_LABEL}) WHERE n.id = '{id_lit}' SET n.status_json = $status_json"),
            &qparams([qp("status_json", status_json)]),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn insert_update_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        kind: &str,
        payload_json: &str,
    ) -> A2aGraphStoreResult<()> {
        let id_lit = escape_cypher_string(id);
        let task_id_key = task_id_storage_key(task_id);
        let task_id_lit = escape_cypher_string(task_id);
        let kind_lit = escape_cypher_string(kind);
        let payload_json_lit = escape_cypher_string(payload_json);
        self.run_cypher_execute(
            &format!(
                "MERGE (n:{TASK_UPDATE_NODE_LABEL} {{id: '{id_lit}'}}) \
                 ON CREATE SET n.task_id = '{task_id_lit}', n.task_id_key = '{task_id_key}', n.seq = {seq}, n.kind = '{kind_lit}', n.payload_json = '{payload_json_lit}' \
                 ON MATCH SET n.task_id = '{task_id_lit}', n.task_id_key = '{task_id_key}', n.seq = {seq}, n.kind = '{kind_lit}', n.payload_json = '{payload_json_lit}'"
            ),
            &empty_params(),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn list_update_nodes(
        &self,
        task_id: &str,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphUpdateNode>> {
        let task_id_key = task_id_storage_key(task_id);
        let rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (n:{TASK_UPDATE_NODE_LABEL}) \
                     WHERE n.task_id_key = $task_id_key \
                     RETURN n.id AS id, n.kind AS kind, n.payload_json AS payload_json \
                     ORDER BY n.seq"
                ),
                &qparams([qp("task_id_key", task_id_key)]),
            )
            .await
            .map_err(|e| e.to_string())?;
        let out: Vec<TaskSubgraphUpdateNode> = rows.iter().filter_map(decode_update_node).collect();
        if !out.is_empty() {
            return Ok(out);
        }

        let legacy_rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (n:{TASK_UPDATE_NODE_LABEL}) \
                     RETURN n.task_id AS task_id, n.id AS id, n.kind AS kind, n.payload_json AS payload_json, n.seq AS seq \
                     ORDER BY n.seq"
                ),
                &empty_params(),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(legacy_rows
            .iter()
            .filter(|row| row.get::<String>("task_id").ok().as_deref() == Some(task_id))
            .filter_map(decode_update_node)
            .collect())
    }

    async fn delete_update_node(&self, id: &str) -> A2aGraphStoreResult<()> {
        let id_lit = escape_cypher_string(id);
        self.run_cypher_execute(
            &format!("MATCH (n:{TASK_UPDATE_NODE_LABEL}) WHERE n.id = '{id_lit}' DELETE n"),
            &empty_params(),
        )
        .await
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_vocabulary::A2aGraphStore;

    use super::{empty_params, escape_cypher_string};
    use crate::GraphqliteStoreBuilder;

    #[test]
    fn escape_cypher_string_escapes_control_whitespace() {
        let escaped = escape_cypher_string("a\tb\nc\rd\\e'f");
        assert_eq!(escaped, "a\\tb\\nc\\rd\\\\e\\'f");
    }

    #[tokio::test]
    async fn legacy_rows_without_task_id_key_remain_readable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prov.db");
        let store = GraphqliteStoreBuilder::file(path)
            .build()
            .expect("build store");

        store
            .run_cypher_execute(
                r#"CREATE (m:A2ATaskMessageSubgraph {
                       id: 'legacy-msg-1',
                       task_id: 'legacy-task-1',
                       seq: 1,
                       message_json: '{"legacy":true}'
                   })"#,
                &empty_params(),
            )
            .await
            .expect("insert legacy message row");

        store
            .run_cypher_execute(
                r#"CREATE (u:A2ATaskUpdateSubgraph {
                       id: 'legacy-upd-1',
                       task_id: 'legacy-task-1',
                       seq: 1,
                       kind: 'status',
                       payload_json: '{"state":"submitted"}'
                   })"#,
                &empty_params(),
            )
            .await
            .expect("insert legacy update row");

        let msgs = <crate::GraphqliteProvenanceStore as A2aGraphStore>::list_message_json(
            &store,
            "legacy-task-1",
        )
        .await
        .expect("read legacy message rows");
        assert_eq!(msgs.len(), 1);

        let updates = <crate::GraphqliteProvenanceStore as A2aGraphStore>::list_update_nodes(
            &store,
            "legacy-task-1",
        )
        .await
        .expect("read legacy update rows");
        assert_eq!(updates.len(), 1);

        let max_msg = <crate::GraphqliteProvenanceStore as A2aGraphStore>::max_message_seq(
            &store,
            "legacy-task-1",
        )
        .await
        .expect("legacy max message seq");
        let max_upd = <crate::GraphqliteProvenanceStore as A2aGraphStore>::max_update_seq(
            &store,
            "legacy-task-1",
        )
        .await
        .expect("legacy max update seq");
        assert_eq!(max_msg, 1);
        assert_eq!(max_upd, 1);
    }
}
