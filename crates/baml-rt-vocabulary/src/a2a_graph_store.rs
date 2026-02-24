use async_trait::async_trait;

pub type A2aGraphStoreResult<T> = std::result::Result<T, String>;

#[derive(Debug, Clone)]
pub struct TaskSubgraphNode {
    pub id: String,
    pub context_id: String,
    pub status_json: String,
    pub metadata_json: String,
    pub extra_json: String,
    pub artifacts_json: String,
}

#[derive(Debug, Clone)]
pub struct TaskSubgraphUpdateNode {
    pub id: String,
    pub kind: String,
    pub payload_json: String,
}

#[async_trait]
pub trait A2aGraphStore: Send + Sync {
    async fn max_task_ord(&self) -> A2aGraphStoreResult<i64>;
    async fn max_message_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64>;
    async fn max_update_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64>;
    async fn get_task_node(&self, id: &str) -> A2aGraphStoreResult<Option<TaskSubgraphNode>>;
    async fn list_task_nodes(
        &self,
        context_id: Option<&str>,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphNode>>;
    async fn upsert_task_node(
        &self,
        node: &TaskSubgraphNode,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()>;
    async fn ensure_task_node(
        &self,
        id: &str,
        context_id: &str,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()>;
    async fn insert_message_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        message_json: &str,
    ) -> A2aGraphStoreResult<()>;
    async fn list_message_json(&self, task_id: &str) -> A2aGraphStoreResult<Vec<String>>;
    async fn set_task_status_json(&self, id: &str, status_json: &str) -> A2aGraphStoreResult<()>;
    async fn insert_update_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        kind: &str,
        payload_json: &str,
    ) -> A2aGraphStoreResult<()>;
    async fn list_update_nodes(
        &self,
        task_id: &str,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphUpdateNode>>;
    async fn delete_update_node(&self, id: &str) -> A2aGraphStoreResult<()>;
}
