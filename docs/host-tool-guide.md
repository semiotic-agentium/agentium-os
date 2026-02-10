# Adding a Host Tool (Rust)

This guide describes how to add a new host tool to the agent platform. Host tools are executed in Rust, are session-based, and are allowlisted by agent manifests.

## 1) Implement the tool

Create a new Rust module in `crates/baml-rt-tools/src/` and implement the `BamlTool` trait.

Key requirements:
- Provide `Input` and `Output` types (serde + schemars + ts-rs).
- Provide a `description()` string (avoid unescaped quotes in descriptions).
- Enforce action-specific required fields at runtime.
- Map tool errors to `BamlRtError`.

If the tool needs secrets:
- Read the secret from `std::env::var` (or cache it in the tool constructor).
- Declare the secret in metadata via `ToolSecretRequirement`.

## 2) Register tool metadata

Expose a metadata function and register it for codegen:

- Use `TypeBasedMetadataBuilder` and `register_tool_metadata!`.
- The tool name should be a namespaced string like `support/<tool>`.

This metadata is used by the builder to generate BAML and TypeScript interfaces.

## 3) Register the tool in the agent runner

Add the tool to `crates/baml-agent-runner/src/main.rs` under the manifest tool registration loop:

- Match on the tool name string.
- Instantiate and register the tool with `runtime_manager.register_tool(...)`.

## 4) Allowlist the tool in the agent manifest

In the agent package `manifest.json`, include the tool name in the `tools` array:

```json
{
  "tools": ["support/<tool>"]
}
```

Host tools MUST be declared in the manifest allowlist, otherwise registration will fail.

## 5) Add the agent prompt + entrypoint

Create or update an agent under `agents/<agent-name>`:

- Add a BAML prompt that emits a `ToolSessionPlan`.
- Add a `src/index.ts` that calls the BAML prompt and formats responses.

## 6) Generate artifacts

Generated files are commonly committed for agent packages:

- `baml_src/generated_tools.baml`
- `src/a2a.ts`
- `src/baml-runtime.d.ts`

Rebuild them when tool schema or A2A types change.

