// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared SurrealQL helpers (storage-safe property maps).

use std::collections::HashMap;

use serde_json::{Map, Value};

/// Convert property keys from colon-style (`a2a:context_id`) to underscore-style (`a2a_context_id`)
/// for storage-safe access in SurrealDB property paths.
pub(crate) fn storage_safe_props(props: &HashMap<String, Value>) -> HashMap<String, Value> {
    props
        .iter()
        .map(|(k, v)| {
            let safe_key = k.replace(':', "_");
            let safe_value = match v {
                Value::Array(_) | Value::Object(_) => match serde_json::to_string(v) {
                    Ok(s) => Value::String(s),
                    Err(e) => {
                        tracing::warn!(error = %e, "storage_safe_props nested serde failed; using null");
                        Value::String("null".to_string())
                    }
                },
                _ => v.clone(),
            };
            (safe_key, safe_value)
        })
        .collect()
}

pub(crate) fn storage_safe_props_sorted_keys(
    props: &HashMap<String, Value>,
) -> Vec<(String, Value)> {
    let m = storage_safe_props(props);
    let mut keys: Vec<String> = m.keys().cloned().collect();
    keys.sort();
    keys.into_iter()
        .filter_map(|k| m.get(&k).map(|v| (k, v.clone())))
        .collect()
}

/// Edge `props` as a single JSON object (replaces whole `props` on UPSERT).
pub(crate) fn edge_props_object(props: &HashMap<String, Value>) -> Value {
    let safe = storage_safe_props(props);
    Value::Object(safe.into_iter().collect::<Map<String, Value>>())
}
