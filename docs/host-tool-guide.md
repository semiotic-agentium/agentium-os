# Adding a Host Tool (Rust)

This guide describes how to add a new **Rust** host tool to Agentium OS. For **agent authoring** (BAML, QuickJS, citations, `StructuredReply`), start with [How to write agents](how-to-write-agents.md).

Host tools are executed in Rust, are session-based, and are allowlisted by agent manifests.

## 1) Implement the tool

Create a new Rust module in `crates/baml-rt-tools/src/` and implement the `BamlTool` trait.

Key requirements:
- Provide `OpenInput`, `Input`, and `Output` types that implement **`serde::Serialize` / `Deserialize`** and the schema traits the registry expects—on `BamlTool`, phase payloads use **`ToolType`** (JSON Schema + TypeScript generation via the tooling pipeline in `baml-rt-tools`). BAML-facing agent types often use **`#[derive(BamlType)]`** (`baml_derive`) where appropriate; tool I/O phases are **`ToolType`** + **`DescribeAction`** where summaries are needed.
- For opaque transport payloads, prefer **`baml_rt_tools::OpaqueJson`** instead of bare `serde_json::Value` on `#[derive(BamlType)]` structs. That keeps generated BAML/TypeScript interfaces explicit while runtime validation still accepts arbitrary JSON.
- The builder does **not** silently degrade unsupported open JSON Schema shapes to `string` anymore. If a payload is intentionally arbitrary JSON, model it explicitly as **`OpaqueJson`** so generated BAML stays honest.
- Provide a `description()` string (avoid unescaped quotes in descriptions).
- Enforce action-specific required fields at runtime.
- Map tool errors to `BamlRtError`.

If the tool needs secrets:
- Read the secret from `std::env::var` (or cache it in the tool constructor).
- Declare the secret in metadata via `ToolSecretRequirement`.

## 2) Register tool metadata

Expose a metadata function and register it for codegen:

- Use `TypeBasedMetadataBuilder` for a metadata fn and a build fn that returns the handler (or `Err` when the tool is not compiled).
- Register with the single mechanism: `register_tool!(metadata_fn, build_fn)`.
- The tool name should be a namespaced string like `support/<tool>`.

This metadata is used by the builder to generate BAML and TypeScript interfaces. The runner registers all manifest tools via `register_manifest_tools` (single inventory); no per-tool match in the runner.

## 3) Allowlist the tool in the agent manifest

In the agent package `manifest.json`, include the tool name in the `tools` array:

```json
{
  "tools": ["support/<tool>"]
}
```

Host tools MUST be declared in the manifest allowlist, otherwise registration will fail.

## 4) Add the agent prompt + entrypoint

Create or update an agent under `agents/<agent-name>`:

- Add a BAML prompt that emits a `ToolSessionPlan`.
- Add a `src/index.ts` that calls the BAML prompt and formats responses.

**Session plan tool resolution:** The runtime resolves which tool to run in two ways. **Primary:** When the agent is built with the builder, it emits `session_plan_functions.json` (function name → session plan type). The runtime uses the **invoking BAML function name** and this manifest to resolve the tool—no `__type` in the plan JSON is required. **Fallback:** If there is no manifest or no entry for the function (e.g. dynamic calls), the plan JSON must include a `__type` field: on the plan object (e.g. `SupportCalculateSessionPlan`) or on each step (e.g. `SupportCalculateOpenStep`). The builder-generated BAML class description mentions `__type` for that fallback case; you only need the model to emit it when not using a built agent package with the manifest.

## 5) Generate artifacts

Generated files are commonly committed for agent packages:

- `baml_src/_baml_runtime.baml`
- `src/baml-runtime.d.ts` (BAML function declarations, A2A session DSL, and agent contract types)

Rebuild them when tool schema or A2A types change.
