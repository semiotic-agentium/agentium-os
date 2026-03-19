//! In-memory deterministic lexical ranked search for tools.
//!
//! Ranking: exact full-name > local-name exact > prefix > token overlap (tags) > token overlap (description).
//! Tie-break: lexical tool name ascending for stable output.

use std::cmp::Ordering;

use crate::tools::{ToolDiscoveryRecord, ToolFunctionMetadata};

/// Normalize query for deterministic matching: lowercase, collapse whitespace, tokenize.
fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split_whitespace()
        .map(|t| t.to_string())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
}

/// Score a tool against a query. Higher is better.
/// Tiers: exact full-name (1000), exact local (800), prefix full (600), prefix local (400), tag overlap (200 + count), description overlap (count), tie-break by name.
fn score_tool(
    metadata: &ToolFunctionMetadata,
    query_tokens: &[String],
    query_lower: &str,
) -> (i64, String) {
    let full_name = metadata.name.to_string();
    let full_lower = full_name.to_lowercase();
    let local_lower = metadata.name.local().as_str().to_lowercase();
    let desc_lower = metadata.description.to_lowercase();
    let tags_lower: Vec<String> = metadata.tags.iter().map(|t| t.to_lowercase()).collect();

    let mut score = 0i64;

    // Exact full-name match
    if full_lower == query_lower {
        score += 10000;
    }
    // Exact local-name match
    else if local_lower == query_lower {
        score += 8000;
    }
    // Prefix: full name starts with query
    else if full_lower.starts_with(query_lower) {
        score += 6000;
    }
    // Prefix: local name starts with query
    else if local_lower.starts_with(query_lower) {
        score += 4000;
    }

    // Token overlap in tags
    for qt in query_tokens {
        for tag in &tags_lower {
            if tag == qt || tag.contains(qt) || qt.contains(tag) {
                score += 200;
                break;
            }
        }
    }

    // Token overlap in description
    for qt in query_tokens {
        if desc_lower.contains(qt) {
            score += 10;
        }
    }

    (score, full_name)
}

/// Rank and return tools from `metadata_list` matching `query`, up to `limit`.
/// Deterministic: same query and corpus yields same order (lexical tie-break on tool name).
/// Lists globally available tools only (no per-agent invokability).
pub fn search_tools(
    metadata_list: &[ToolFunctionMetadata],
    query: &str,
    limit: usize,
) -> Vec<ToolDiscoveryRecord> {
    let query_trimmed = query.trim();
    let query_lower = query_trimmed.to_lowercase();
    let query_tokens = tokenize(query_trimmed);

    let mut scored: Vec<(i64, String, ToolDiscoveryRecord)> = metadata_list
        .iter()
        .map(|m| {
            let (score, name) = score_tool(m, &query_tokens, &query_lower);
            let record = ToolDiscoveryRecord::from_metadata(m);
            (score, name, record)
        })
        .collect();

    scored.sort_by(|a, b| match b.0.cmp(&a.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });

    scored.into_iter().take(limit).map(|(_, _, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolName, ToolOrigin, ToolTypeSpec};

    fn meta(name: &str, description: &str, tags: Vec<String>) -> ToolFunctionMetadata {
        let n = ToolName::parse(name).unwrap();
        ToolFunctionMetadata {
            name: n,
            class_name: "Test".to_string(),
            description: description.to_string(),
            open_input_schema: serde_json::Value::Object(serde_json::Map::new()),
            input_schema: serde_json::Value::Object(serde_json::Map::new()),
            output_schema: serde_json::Value::Object(serde_json::Map::new()),
            open_input_type: ToolTypeSpec {
                name: "()".to_string(),
                ts_decl: None,
            },
            input_type: ToolTypeSpec {
                name: "{}".to_string(),
                ts_decl: None,
            },
            output_type: ToolTypeSpec {
                name: "{}".to_string(),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: vec![],
            access: None,
            tags,
            secret_requests: vec![],
            config: None,
            config_bundle: None,
            origin: ToolOrigin::Host,
            projection_semantics: None,
            session_policy: crate::tools::SessionPolicy::default(),
            event_sources: vec![],
        }
    }

    #[test]
    fn exact_full_name_ranks_first() {
        let tools = vec![
            meta("support/calculate", "Calculator", vec![]),
            meta("support/weather", "Weather", vec![]),
        ];
        let out = search_tools(&tools, "support/calculate", 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name.to_string(), "support/calculate");
    }

    #[test]
    fn determinism_tie_break_lexical() {
        let tools = vec![meta("b/bb", "Desc", vec![]), meta("a/aa", "Desc", vec![])];
        let out = search_tools(&tools, "x", 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name.to_string(), "a/aa");
        assert_eq!(out[1].name.to_string(), "b/bb");
    }

    #[test]
    fn tokenize_normalizes() {
        let t = tokenize("  Foo   Bar  ");
        assert_eq!(t, vec!["foo", "bar"]);
    }
}
