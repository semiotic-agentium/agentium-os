# External tool snapshot lite plan

## Recommendation

Prefer this lite plan over Plan A. Near-term goal: **schemas generated/discovered from tool implementation and approved/snapshotted like MCP**.

Existing surface already has `ExternalToolMetadata`, `ToolRuntime`, `tool/describe`, `tool/schema`, `tool/invoke`, process/sandbox handlers, and builder projections. Missing piece is lifecycle + snapshot persistence. Plan A is better later if product needs multi-capability extensions, webhooks, or package-level contribution registry — it would introduce `contributes.invokables[]`, `extension/describe`/`extension/invoke`, naming churn, and duplicate DTO layers.

## Current limitations

- `tool-metadata.json` is authoritative for schemas; tool authors hand-write `schemas.input`/`schemas.output`.
- `ExternalMetadataCatalog` reads local dirs via `BAML_EXTERNAL_TOOLS_DIR`; no approval gate.
- `DevModeResolver` computes digest from binary + full metadata file; validates `tool/describe` but does not call `tool/schema`.
- `StdioSubprocessInvoker` supports `describe` and `invoke`; `METHOD_SCHEMA` constant exists but schema call is not implemented.
- `SandboxTool` exposes `schema() -> Option<ToolSchemaResult>` and dispatches `tool/schema`; host-side `SandboxInvoker` does not expose or call it.
- Builder consumes approved MCP snapshots via `McpSnapshotCatalog`; no external-tool equivalent.
- Repository has MCP registry types/routes/store; no external-tool registry.
- MCP has refresh/drift lifecycle; external tools have digest checks only.

## Constraints and non-goals

- Keep `ToolName` (`bundle/local`) and all existing external-tool protocol names.
- Keep `baml-sandbox-protocol` lean: wire structs only, no `baml-rt-tools` dep.
- No `extension/*` methods, `contributes.invokables[]`, `extension/describe`, or `extension/invoke`.
- No three-part `PlatformToolName` unless collision evidence appears.
- One package = one platform tool. No multi-invokable in this phase.
- No webhook/event-source implementation.
- No registry-required runtime for dev mode; legacy `DevModeResolver` path remains.
- No duplicate `ExtensionTool` trait or adapter dispatch loop.
- `--offline` discovery flag: future work, not in any phase here.

## Data model

### Author manifest: `tool-manifest.json`

Operator-owned fields only; no schemas.

```json
{
  "tool_abi_version": "1",
  "name": "support/weather",
  "description": "Fetch weather",
  "bundle": "support",
  "local_name": "weather",
  "access_level": "read",
  "tags": ["weather"],
  "invocation_mode": "single_shot",
  "session_policy": "strict",
  "secrets": ["WEATHER_API_KEY"],
  "secret_scope": "send",
  "capabilities": {"network": {"allow_hosts": ["api.weather.example"]}},
  "config_bundle": "support_weather",
  "runtime": {"kind": "process", "command": ["./tool-server"]},
  "coordination": null
}
```

**Manifest resolution order:** if `tool-manifest.json` exists, use it. Else read legacy `tool-metadata.json`; if `schemas` present, treat as unsnapshotted legacy — migration can seed first snapshot.

`ExternalToolManifest` Rust type: subset of `ExternalToolMetadata` fields; no `schemas` field.

### Approved snapshot

`ExternalToolSnapshot` wraps `ExternalToolMetadata` plus provenance/approval metadata. Approval fields are excluded from `snapshot_digest` so approving a pending snapshot does not change its content identity.

```json
{
  "snapshot_schema_version": 1,
  "source": "external_tool",
  "snapshot_digest": "sha256:...",
  "tool": { /* full ExternalToolMetadata including schemas */ },
  "describe": {
    "protocol_version": "1",
    "supported_methods": ["tool/describe", "tool/schema", "tool/invoke"],
    "max_payload_bytes": null,
    "schema_digest": "sha256:..."
  },
  "digests": {
    "manifest_digest": "sha256:...",
    "schema_digest": "sha256:...",
    "runtime_digest": "sha256:...",
    "snapshot_digest": "sha256:..."
  },
  "approval": {
    "state": "pending",
    "owner": null,
    "reviewed_at": null,
    "expires_at": null
  },
  "created_at": "..."
}
```

`coordination_baml`: if coordination BAML is referenced by path, inline the file content into the snapshot at discovery time so builder is independent of source dir.

### Digest inputs (canonical JSON / JCS)

- `schema_digest`: `{"input": schema.input, "output": schema.output}` — same as current `metadata_schema_digest`.
- `manifest_digest`: all manifest fields except secret values; includes name, description, tags, access, invocation/session policy, secret names/scope, capabilities, config_bundle, runtime declaration, coordination pointer.
- `runtime_digest`:
  - process: command argv + binary bytes + mode bits if command resolves to a local package path. For PATH-resolved or external binaries, use command string only and emit a warning that the digest is not content-pinned.
  - sandbox bind: runtime spec + bind rootfs digest if available; else path + lock digest with non-portable warning.
  - sandbox OCI: runtime spec + image digest ref.
- `snapshot_digest`: full snapshot JSON excluding the `approval` object.

### Approval states

Reuse `ApprovalRecord` after extracting it as a generic type from the MCP module (Phase 1). States: `pending`, `approved`, `rejected`, `stale`.

- `pending`: discovered; not projectable by builder/runner.
- `approved`: active and projectable.
- `stale`: previously approved; live snapshot drifted. Builder/runner keep latest approved non-stale. Stale version not selected as active.
- `rejected`: retained for audit, never active.

### Registry record shape (mirrors MCP)

- `ExternalToolRegistryTool`: `tool_name`, `tenant_id?`, `display_name?`, `created_at`, `latest_version?`
- `ExternalToolRegistryToolVersion`: `tool_name`, `version`, `snapshot_digest`, `manifest_digest`, `schema_digest`, `runtime_digest`, `protocol_version`, `runtime_json`, `secrets_json`, `capabilities_json`, `approval_state`, `owner?`, `reviewed_at?`, `expires_at?`, `created_at`, `stale_at?`
- `ExternalToolSnapshotBlob`: `snapshot_digest`, `snapshot_json`

### File cache layout

```
<root>/external-tools/
  tools/<tool_slug>/tool-snapshot.json     # latest approved
  pending/<tool_slug>/<snapshot_digest>.json
```

`<tool_slug>`: tool name with `/` replaced by `__` (e.g. `support__weather`).

`BAML_EXTERNAL_TOOL_CACHE_DIR` for test/offline fixture input. Production runners prefer repository client config (`BAML_REPOSITORY_URL`). Cache remains supported fallback.

## Process discovery flow

1. CLI reads `tool-manifest.json` (or legacy `tool-metadata.json`) from `<dir>`.
2. Resolve process runtime command via `ToolRuntime::Process` rules.
3. Construct `StdioSubprocessInvoker` with working dir/env policy.
4. Call `tool/describe`. Verify:
   - `protocol_version == PROTOCOL_VERSION`
   - `tool_name` matches manifest `name`
   - `supported_methods` contains both `tool/schema` and `tool/invoke`. If `tool/schema` absent, fail discovery with clear error — no fallback to hand-authored schema.
5. Call `schema()` invoker method (`METHOD_SCHEMA`). Signature:
   ```rust
   async fn schema(&self, tool: &ToolName, timeout: Duration) -> Result<ToolSchemaResult, InvokerError>;
   ```
   Verify:
   - `tool_name` matches
   - `content_type == "application/schema+json"`
   - `content_digest` equals canonical digest of `{"input": ..., "output": ...}`
   - if `describe.schema_digest` present, it equals `content_digest`
6. Merge manifest + schema → `ExternalToolMetadata`. Inline coordination BAML. Compute all digests. Create pending snapshot.
7. Print summary: name, description, access, runtime kind, secrets, capabilities, schema digest, input/output schema preview (diff vs existing approved if any).
8. Prompt approval (skip with `--yes`). On approval, write snapshot to registry and/or `--cache-dir`.

## Sandbox discovery flow

### OCI/bind runtime path

1. Read manifest `kind=sandbox`.
2. Build `SandboxSpec` via existing spec builder.
3. Create temporary discovery sandbox. Call `tool/describe` then `tool/schema` via `SandboxInvoker`.
4. Merge manifest + schema → snapshot.
5. Tear down sandbox (always, including on error — use drop guard).

### Adapter sidecar optimization

If bind/rootfs bundle already includes `tool-bundle.json` with schema, CLI may read it as optimization. Still call `tool/schema` before approval to validate — sidecar shortcut never bypasses live validation.

## Builder flow

`build_builder_catalog()` source order (unchanged collision check by `ToolName`):

1. inventory
2. legacy `BAML_EXTERNAL_TOOLS_DIR` metadata (compat/dev via `ExternalMetadataCatalog`)
3. approved external snapshots from `BAML_EXTERNAL_TOOL_CACHE_DIR` → `ExternalToolSnapshotCatalog`
4. approved external snapshots from repository → `ExternalToolRegistryCatalog`
5. MCP snapshots

Both `ExternalToolSnapshotCatalog` (file cache) and `ExternalToolRegistryCatalog` (repository) call `build_tool_metadata(PathBuf::new(), &snapshot.tool, &tool_name)` — empty path is valid for registry-sourced snapshots where source dir is unavailable. Coordination BAML is already inlined in snapshot.

## Runner flow

Add `ExternalRegistryResolver` alongside `DevModeResolver`.

- Reads approved snapshots from repository/database or file cache.
- Constructs `ProcessToolHandler`/`SandboxToolHandler` from snapshot runtime config.
- Projects through `build_tool_metadata()` → same `ToolFunctionMetadata` as builder.
- At startup/resolve: `manifest_digest` and `runtime_digest` must exactly match approved snapshot. Mismatch = hard error.
- Schema drift (live `tool/describe` `schema_digest` differs from snapshot): emit stale event to registry if available; continue using approved snapshot for codegen contract. Do not silently switch to pending schema.
- If marking stale would leave no approved version, keep last-known-approved cache and surface hard warning.

`DevModeResolver` stays for local dev. Snapshot mode = production default.

## Refresh/drift behavior

`external-tool refresh <name> [--dir <dir>]`:

1. Locate approved snapshot + source config.
2. Re-run discovery (describe + schema).
3. Compute new digests. Compare:
   - same `snapshot_digest`: no-op, print unchanged.
   - any digest changed: write pending version; old approved stays active.
4. Print diff. Prompt approval. If approved, new version = latest approved; old version = audit history. If rejected, old approved unchanged.

## CLI commands

All under `cargo agent-platform external-tool`.

Common flags: `--repository-url`, `--cache-dir`, `--runner-token`, `--json`, `--yes`.

### `external-tool enable <dir>`

Discover from local package → create pending snapshot → print summary → prompt approval → write to registry and/or `--cache-dir`.

### `external-tool inspect <name>`

Show approved + pending versions, digest summary, approval metadata. Flags: `--version`.

### `external-tool refresh <name>`

Re-discover from snapshot `source_ref` or `--dir`. Compare; create pending if changed. Prompt approval.

### Future

`tool enable <dir>` alias after UX decision. Do not overload existing `new-tool` scaffolding in first PR.

## Migration from hand-written metadata

1. Legacy `tool-metadata.json` with `schemas` accepted forever (dev/backcompat).
2. Explicit migration only — no auto-migration on first catalog load.
3. `external-tool migrate <dir>` (or `enable <dir> --write-manifest`): writes `tool-manifest.json` without schemas, runs live discovery, creates first approved snapshot.
4. If live schema differs from hand-written schema: print diff, require explicit approval, snapshot uses live schema, legacy file left in git for operator to clean up.
5. New scaffolded external tools: implement `tool/schema`, generated manifest omits schemas.

## Phased checklist

### Phase 0: protocol plumbing ✅

- [x] Add `schema()` to `ExternalInvoker` / `ToolInvoker`: `async fn schema(&self, tool: &ToolName, timeout: Duration) -> Result<ToolSchemaResult>`
- [x] Implement in `StdioSubprocessInvoker` reusing JSON-RPC call helpers.
- [x] Implement in `SandboxInvoker` using existing `json_rpc_call`.
- [x] Error if `tool/schema` not in `supported_methods` (no fallback). ← caller responsibility; invoker propagates JSON-RPC errors.
- [x] Tests: missing `tool/schema` error propagation, malformed result, sandbox round-trip (4 tests, all green).

### Phase 1: manifest + snapshot types

- [x] Extract generic `ApprovalRecord` from MCP module; replace MCP-specific type with it.
- [x] Add `ExternalToolManifest` (slim type, no schemas).
- [x] Add `ExternalToolSnapshot` with approval fields, digests, describe info.
- [x] Add `ExternalApprovalState` enum reusing shared approval states.
- [x] Add digest helpers (JCS canonical, manifest_digest, schema_digest, runtime_digest, snapshot_digest excluding approval).
- [x] Support reading `tool-manifest.json`; keep legacy `tool-metadata.json` with optional schemas.
- [x] Discovery: merge manifest + `ToolSchemaResult` + coordination BAML inline → `ExternalToolMetadata` → `ExternalToolSnapshot`.

### Phase 2: file-cache catalog + resolver

- [x] Add `external_tool_cache` module (layout: `tools/<slug>/tool-snapshot.json`, `pending/<slug>/<digest>.json`).
- [x] `ExternalToolSnapshotCatalog`: reads `BAML_EXTERNAL_TOOL_CACHE_DIR`, projects approved only.
- [x] Cache-backed `ExternalRegistryResolver` for offline/dev.
- [x] Call `build_tool_metadata(PathBuf::new(), ...)` for registry-sourced snapshots.
- [x] Tests: approved projected; pending and stale filtered out; collision with inventory. (approved/pending/stale and tamper rejection covered; inventory-collision branch implemented; direct inventory-collision unit coverage limited because `baml-rt-tools` unit inventory is empty.)

Phase 2 complete. Follow-up note: stale/drift event emission remains Phase 5 lifecycle work unless CLI refresh owns the emission in Phase 3. Cache catalog/resolver currently reject stale records and tampered approved snapshots but do not perform live schema drift checks.

### Phase 3: CLI enable/inspect/refresh (cache path)

- [ ] `external-tool enable <dir>`: discover → pending snapshot → approval prompt → write cache.
- [ ] `external-tool inspect <name>`: show approved/pending/stale with digests.
- [ ] `external-tool refresh <name> --dir <dir>`: re-discover → compare → pending if changed → prompt.
- [ ] Schema diff/digest summary display.
- [ ] Tests: `--yes` approves; abort leaves cache unchanged; `--json` raw output.

### Phase 4: repository registry write/read

- [ ] External snapshot registry types in `baml-rt-repository`.
- [ ] Store trait methods + sqlite/surreal/in-memory implementations.
- [ ] HTTP routes analogous to `/mcp/snapshots/import`.
- [ ] CLI posts approved snapshots to registry; still supports `--cache-dir` alongside.
- [ ] Builder `ExternalToolRegistryCatalog` fetches approved snapshots from registry.

### Phase 5: runner registry resolver hardening

- [ ] Complete `ExternalRegistryResolver` reading repository snapshots.
- [ ] Build process/sandbox handlers from approved snapshot runtime.
- [ ] Enforce manifest + runtime digest at resolve time (exact match).
- [ ] Emit stale/drift lifecycle events on schema mismatch.
- [ ] Keep `DevModeResolver` as legacy local path.

### Phase 6: migration + scaffolding

- [ ] `external-tool migrate <dir>`: write `tool-manifest.json`, discover, create first approved snapshot, print diff vs hand-written schema.
- [ ] Update scaffolded external tool templates: implement `tool/schema`, omit schemas from manifest.
- [ ] Docs: author manifest vs approved snapshot lifecycle.

## Tradeoffs vs Plan A

| | Lite plan | Plan A |
|---|---|---|
| Diff size | Small | Large |
| Naming churn | None | High (`extension/*`) |
| Multi-invokable | Not now | Supported |
| DTO duplication | Low | High |
| Approval/cache code | Shared with MCP via extracted helpers | Independent |
| Runtime digest | Weaker for PATH binaries | OCI-pinnable |

Main cost: if product needs N invokables per package soon, migration needed. Mitigate by keeping `ExternalToolSnapshot` additive (`event_sources: []`, `webhooks: []` reserved fields).
