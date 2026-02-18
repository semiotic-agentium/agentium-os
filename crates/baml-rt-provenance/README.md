# baml-rt-provenance

Provenance capture and storage for the BAML agent runtime. Events are normalized to W3C PROV and persisted in a GraphQLite-backed graph (SQLite + Cypher).

## Vocabulary

Keys, relations, and identifiers use the crate vocabulary consistently. All attribute names (e.g. `a2a:context_id`, `prov:role`), relation types (e.g. `USED`, `WAS_GENERATED_BY`), and PROV/A2A terms come from [`vocabulary`](src/vocabulary.rs) (`prov::`, `a2a::`, `prov_relations::`, `a2a_relations::`, `semantic_labels::`, etc.). The **graph node identity property** is [`vocabulary::graph::NODE_ID`](src/vocabulary.rs) (`"id"`), matching [GraphQLite’s convention](https://github.com/colliery-io/graphqlite/blob/main/bindings/rust/src/graph/nodes.rs): the high-level API (`upsert_node`, `has_node`, `get_node`) uses `id` for MERGE/MATCH. Storage-safe keys (e.g. `a2a_context_id`) are derived from vocabulary keys by replacing `:` with `_` for GraphQLite.

## Parameterized Cypher (no manual escaping)

**All Cypher that embeds runtime values uses parameterized queries.** Values are bound by the GraphQLite driver via `cypher_with_params(query, &params)`, which prevents injection and eliminates the need for manual escaping.

- **Tool index** ([`tool_index.rs`](src/tool_index.rs)): `MERGE` for `ToolFunction` nodes uses a `params` object (e.g. `$name`, `$description`, …) and `conn.cypher_with_params(&query, &params)`.
- **Provenance write path** ([`cypher_build.rs`](src/cypher_build.rs), [`graphqlite_store.rs`](src/graphqlite_store.rs)): `build_query_with_key_style_params()` returns `(query_string, params)`. The store worker runs `conn.cypher_with_params(&query, &params)` for each write. No `cypher_string_literal` or ad-hoc escaping of user/content strings.
- **Provenance read path** ([`graph_model.rs`](src/graph_model.rs), [`graphqlite_store.rs`](src/graphqlite_store.rs)): `message_query_storage_safe_params(context)` and `tool_query_storage_safe_params(context)` return `(query, params)` with `$context`; the store uses `run_cypher_with_params(&query, &params)` for `context_messages` and `conversation_context`.

Example pattern:

```rust
let params = json!({"name": "Alice", "age": 30});
let query = "CREATE (n:Person {name: $name, age: $age})";
conn.cypher_with_params(query, &params)?;
```

When adding new Cypher that embeds values, use this pattern everywhere and document it here.
