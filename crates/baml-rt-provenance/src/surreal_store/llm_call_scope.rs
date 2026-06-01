// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Resolve persisted LLM call scope from graph nodes (cross-batch PromptRejected linking).

use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{error::Result, surreal_tables::TBL_NODE};

/// Parse `llm_call:{scope_key}:{ordinal}` node id written by the normalizer.
pub(super) fn parse_llm_call_node_id(node_id: &str) -> Option<(String, u64)> {
    let rest = node_id.strip_prefix("llm_call:")?;
    let (scope_key, ord_str) = rest.rsplit_once(':')?;
    let ordinal: u64 = ord_str.parse().ok()?;
    if scope_key.is_empty() {
        return None;
    }
    Some((scope_key.to_string(), ordinal))
}

impl SurrealProvenanceStore {
    /// Lookup `(scope_key, ordinal)` for a completed LLM call by its event activity anchor.
    pub(super) async fn resolve_llm_call_scope_ordinal_by_event_anchor(
        &self,
        event_anchor: &str,
    ) -> Result<Option<(String, u64)>> {
        let anchor = event_anchor.trim();
        if anchor.is_empty() {
            return Ok(None);
        }
        let q = format!(
            "SELECT node_id FROM {TBL_NODE} \
             WHERE label = 'LlmCall' AND props.a2a_activity_anchor = $anchor LIMIT 1"
        );
        let response = self
            .db
            .query(&q)
            .bind(("anchor", anchor.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let node_id = rows.first().and_then(|r| {
            r.get("node_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
        Ok(node_id.and_then(parse_llm_call_node_id))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_llm_call_node_id;

    #[test]
    fn parse_llm_call_node_id_roundtrip() {
        let scope = "ctx-1-2:dispatch-unit-abc:00000000-0000-0000-0000-000000000000";
        let node_id = format!("llm_call:{scope}:3");
        let (sk, ord) = parse_llm_call_node_id(&node_id).expect("parse");
        assert_eq!(sk, scope);
        assert_eq!(ord, 3);
    }
}
