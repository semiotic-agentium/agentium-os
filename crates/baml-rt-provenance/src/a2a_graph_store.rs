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

#[async_trait]
impl A2aGraphStore for GraphqliteProvenanceStore {
    async fn max_task_ord(&self) -> A2aGraphStoreResult<i64> {
        let rows = self
            .run_cypher_read(
                &format!("MATCH (t:{TASK_NODE_LABEL}) RETURN coalesce(max(t.ord), 0) AS max_ord"),
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

    async fn max_seq_for_label(&self, label: &str, task_id: &str) -> A2aGraphStoreResult<i64> {
        let query = format!(
            "MATCH (n:{label}) WHERE n.task_id = $task_id RETURN coalesce(max(n.seq), 0) AS max_seq"
        );
        let rows = self
            .run_cypher_read(&query, &qparams([qp("task_id", task_id)]))
            .await
            .map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .next()
            .and_then(|row| row.get::<i64>("max_seq").ok())
            .unwrap_or(0))
    }

    async fn get_task_node(&self, id: &str) -> A2aGraphStoreResult<Option<TaskSubgraphNode>> {
        let rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (t:{TASK_NODE_LABEL}) WHERE t.id = $id \
                     RETURN t.id AS id, t.context_id AS context_id, t.status_json AS status_json, \
                            t.metadata_json AS metadata_json, t.extra_json AS extra_json, t.artifacts_json AS artifacts_json"
                ),
                &qparams([qp("id", id)]),
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
                    "MATCH (t:{TASK_NODE_LABEL}) WHERE t.context_id = $context_id \
                     RETURN t.id AS id, t.context_id AS context_id, t.status_json AS status_json, \
                            t.metadata_json AS metadata_json, t.extra_json AS extra_json, t.artifacts_json AS artifacts_json \
                     ORDER BY t.ord"
                ),
                qparams([qp("context_id", cid)]),
            )
        } else {
            (
                format!(
                    "MATCH (t:{TASK_NODE_LABEL}) \
                     RETURN t.id AS id, t.context_id AS context_id, t.status_json AS status_json, \
                            t.metadata_json AS metadata_json, t.extra_json AS extra_json, t.artifacts_json AS artifacts_json \
                     ORDER BY t.ord"
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
        self.run_cypher_execute(
            &format!(
                "MERGE (t:{TASK_NODE_LABEL} {{id: $id}}) \
                 ON CREATE SET t.ord = $ord \
                 SET t.context_id = $context_id, \
                     t.status_json = $status_json, \
                     t.metadata_json = $metadata_json, \
                     t.extra_json = $extra_json, \
                     t.artifacts_json = $artifacts_json"
            ),
            &qparams([
                qp("id", node.id.clone()),
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
        self.run_cypher_execute(
            &format!(
                "MERGE (t:{TASK_NODE_LABEL} {{id: $id}}) \
                 ON CREATE SET t.context_id = $context_id, t.status_json = '', t.metadata_json = '{{}}', t.extra_json = '{{}}', t.artifacts_json = '[]', t.ord = $ord"
            ),
            &qparams([qp("id", id), qp("context_id", context_id), qp("ord", ord_if_create)]),
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
        self.run_cypher_execute(
            &format!(
                "MERGE (m:{TASK_MESSAGE_NODE_LABEL} {{id: $id}}) \
                 SET m.task_id = $task_id, m.seq = $seq, m.message_json = $message_json"
            ),
            &qparams([
                qp("id", id),
                qp("task_id", task_id),
                qp("seq", seq),
                qp("message_json", message_json),
            ]),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn list_message_json(&self, task_id: &str) -> A2aGraphStoreResult<Vec<String>> {
        let rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (m:{TASK_MESSAGE_NODE_LABEL}) WHERE m.task_id = $task_id \
                     RETURN m.message_json AS message_json ORDER BY m.seq"
                ),
                &qparams([qp("task_id", task_id)]),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .filter_map(|row| row.get::<String>("message_json").ok())
            .collect())
    }

    async fn set_task_status_json(&self, id: &str, status_json: &str) -> A2aGraphStoreResult<()> {
        self.run_cypher_execute(
            &format!("MATCH (t:{TASK_NODE_LABEL} {{id: $id}}) SET t.status_json = $status_json"),
            &qparams([qp("id", id), qp("status_json", status_json)]),
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
        self.run_cypher_execute(
            &format!(
                "MERGE (u:{TASK_UPDATE_NODE_LABEL} {{id: $id}}) \
                 SET u.task_id = $task_id, u.seq = $seq, u.kind = $kind, u.payload_json = $payload_json"
            ),
            &qparams([
                qp("id", id),
                qp("task_id", task_id),
                qp("seq", seq),
                qp("kind", kind),
                qp("payload_json", payload_json),
            ]),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn list_update_nodes(
        &self,
        task_id: &str,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphUpdateNode>> {
        let rows = self
            .run_cypher_read(
                &format!(
                    "MATCH (u:{TASK_UPDATE_NODE_LABEL}) WHERE u.task_id = $task_id \
                     RETURN u.id AS id, u.kind AS kind, u.payload_json AS payload_json \
                     ORDER BY u.seq"
                ),
                &qparams([qp("task_id", task_id)]),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(rows.iter().filter_map(decode_update_node).collect())
    }

    async fn delete_update_node(&self, id: &str) -> A2aGraphStoreResult<()> {
        self.run_cypher_execute(
            &format!("MATCH (u:{TASK_UPDATE_NODE_LABEL} {{id: $id}}) DELETE u"),
            &qparams([qp("id", id)]),
        )
        .await
        .map_err(|e| e.to_string())
    }
}
