use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;

pub type GraphQueryParams = serde_json::Map<String, Value>;
pub type GraphRow = HashMap<String, Value>;
pub type GraphStoreResult<T> = std::result::Result<T, String>;

#[async_trait]
pub trait GraphStore: Send + Sync {
    async fn query(
        &self,
        query: &str,
        params: &GraphQueryParams,
    ) -> GraphStoreResult<Vec<GraphRow>>;
    async fn execute(&self, query: &str, params: &GraphQueryParams) -> GraphStoreResult<()>;
}
