<!-- doc-type: assertion -->

# LLM → host tool input JSON (design note)

## Problem

Session **Send** payloads are JSON produced by an LLM from BAML / `{{ ctx.output_format }}` / TypeScript-flavoured schemas. **BAML/jsonish parses that JSON first** into plan types; then the **host** deserializes tool args into Rust (often with `serde(rename_all = "snake_case")` on enums and fields).

If the model emits **snake_case enum strings** (e.g. `"resolve_users"`) but the BAML IR only lists **PascalCase** variants without `@alias`, jsonish may **reject** the intended union arm (e.g. `GetThreadRepliesInput`) and fall through to another arm of `SlackInput` that ignores unknown keys — producing **silent wrong tool calls** (e.g. `ListConversations` with `kinds: []`) long before Rust `serde` runs.

Models frequently emit:

- **PascalCase** enum discriminants matching TS or schema *titles* (e.g. `"ResolveUsers"`).
- **snake_case** keys (often correct) with **wrong** enum *values* for serde’s configured rename rule (e.g. `"resolve_users"` vs `"ResolveUsers"`).

`#[serde(untagged)]` unions (polymorphic tool inputs) then fail entirely if **no** variant deserializes, producing opaque errors (`data did not match any variant of untagged enum …`) even when the *intent* (e.g. `GetThreadReplies`) is obvious.

**Prompt and schema copy** (“use snake_case”) reduces frequency but does **not** guarantee compliance; **the durable fix at the boundary** is to accept both shapes in deserialization (e.g. `#[serde(alias = "ResolveUsers")]` on the Rust variant).

## Current practice (2025)

- Per-tool crates (`baml-tools-slack`, etc.) add **explicit `serde::alias`** attributes on enums that appear in LLM-visible Send JSON.
- For types that flow through **BAML unions** first, add **`#[baml(alias = "snake_case_value")]`** on Rust enum variants so `regen_fixtures` emits matching **`@alias("...")`** in `_baml_runtime.baml` — jsonish then accepts the same strings serde accepts.
- **`@baml(description)`** on struct fields documents the canonical JSON form and nudges the model.
- **Unit tests** with golden JSON strings (LLM-shaped) catch regressions.

## Future generalisation (architecture options)

Roughly ordered from **lowest** to **highest** integration cost.

### 1. Shared guideline + optional derive helper (recommended first step)

- Document in this crate (this file) and in **baml-tool-derive** / tool authoring docs: *any enum exposed in tool `Input` / `OpenInput` SHOULD list PascalCase aliases for each variant name as emitted by TS/BAML labels.*
- Add a small **`baml_rt_tools::serde_llm`** (or `baml-derive` proc-macro) helper:
  - **`#[derive(LlmFriendlyEnum)]`** or attribute **`#[llm_enum_aliases]`** that expands to `#[serde(alias = "…")]` for each variant’s PascalCase mirror (and optionally `camelCase` if we see it in the wild).
- **Risk:** aliases must not collide across variants; macro should generate only safe pairs.

### 2. Normalise-on-wire layer (pre–`serde_json`)

- Before `deserialize` into `Input`, run a **JSON value transform** that rewrites known enum string paths (from a registry or JSON Schema) from PascalCase → snake_case.
- **Pros:** centralised; tool types stay idiomatic Rust only.
- **Cons:** must stay in sync with schema; path-specific logic for nested / untagged unions is fragile; security/perf surface for large payloads.

### 3. `serde` “visitor” deserializers for high-value types

- Implement **`deserialize_with`** on specific fields or newtypes (e.g. `LlmSlackUserResolution`) that accept a fixed set of strings.
- **Pros:** precise, testable.
- **Cons:** boilerplate; doesn’t scale to every tool without codegen.

### 4. Contract tests from JSON Schema

- Generate or snapshot **JSON Schema** for each tool’s Send input; fuzz / property-test that **both** snake_case and PascalCase enum encodings deserialize to the same Rust value where aliases are intended.
- Fits well next to **`tool_schema`** / **`json_schema_value`** in this crate.

### 5. BAML / codegen alignment (longer horizon)

- If the BAML or TS emitter can guarantee **one canonical JSON encoding** for enums (always snake_case string values), hosts could rely on prompts alone — but **in practice** models still drift; keeping **aliases at the Rust boundary** remains cheap insurance.
- Optional: emit **few-shot JSON examples** next to types in IR (already partially done via `@description`).

## Ownership

| Concern | Owner |
|--------|--------|
| This design note | `baml-rt-tools` |
| Concrete aliases on a tool’s types | Each `baml-tools-*` crate |
| Derive / shared deserializers | `baml-rt-tools` + optionally `baml-tool-derive` |
| Golden JSON tests | Same crate as the tool types |

## References

- Example fix: `baml-tools-slack` — `SlackUserResolutionMode` and related enums with `serde(alias = …)` plus a unit test deserializing LLM-shaped `GetThreadReplies` JSON.
