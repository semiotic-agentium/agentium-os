use std::collections::HashMap;

use async_trait::async_trait;
use baml_rt_vocabulary::{GraphQueryParams, GraphRow, GraphStore, GraphStoreResult};
use serde_json::Value;

use crate::{GraphqliteProvenanceStore, graphqlite_store::GraphCypherResult};

fn cypher_rows_to_json_rows(rows: &GraphCypherResult) -> Vec<GraphRow> {
    let columns = rows.columns();
    rows.iter()
        .map(|row| {
            let mut out = HashMap::with_capacity(columns.len());
            for col in columns {
                if let Ok(v) = row.get::<String>(col) {
                    out.insert(col.to_string(), Value::String(v));
                    continue;
                }
                if let Ok(v) = row.get::<i64>(col) {
                    out.insert(col.to_string(), Value::Number(v.into()));
                    continue;
                }
                if let Ok(v) = row.get::<bool>(col) {
                    out.insert(col.to_string(), Value::Bool(v));
                    continue;
                }
                if let Ok(v) = row.get::<f64>(col)
                    && let Some(n) = serde_json::Number::from_f64(v)
                {
                    out.insert(col.to_string(), Value::Number(n));
                    continue;
                }
                out.insert(col.to_string(), Value::Null);
            }
            out
        })
        .collect()
}

#[async_trait]
impl GraphStore for GraphqliteProvenanceStore {
    async fn query(
        &self,
        query: &str,
        params: &GraphQueryParams,
    ) -> GraphStoreResult<Vec<GraphRow>> {
        self.run_cypher_read(query, params)
            .await
            .map(|rows| cypher_rows_to_json_rows(&rows))
            .map_err(|e| e.to_string())
    }

    async fn execute(&self, query: &str, params: &GraphQueryParams) -> GraphStoreResult<()> {
        self.run_cypher_execute(query, params)
            .await
            .map_err(|e| e.to_string())
    }
}
