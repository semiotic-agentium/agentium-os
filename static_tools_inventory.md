# Static Tool Inventory Decoupling Plan

## Goal

Make `cargo-agent-platform regen` use runner/repository static-tool metadata as source of truth instead of local CLI link-time `inventory`.

User outcome:

- User downloads one thin `agent-platform` binary.
- Binary can regenerate agents using any static tools linked into target runner, including private/internal tools not shipped in CLI.
- Regen/deploy failures report runner capability mismatch, not stale local CLI feature mismatch.

## Verification Findings & Plan Corrections (2026-06-18)

Plan premises checked against code. All correct, plus findings that change Phase 1, Phase 2, Phase 4/5, Phase 7.

### Premises confirmed

- CLI links all tools: `crates/cargo-agent-platform/Cargo.toml:20` (`baml-tool-links … features=["all-tools"]`), `force_link_all_tools!()` at `crates/cargo-agent-platform/src/main.rs:612`.
- Builder catalog starts local: `build_builder_catalog_with_roots(mcp_root, external_cache_root)` → `InventoryCatalog::new()` at `crates/baml-rt-tools/src/external_tools/metadata_catalog.rs:39,43`. Collision detection across inventory/external/MCP already done here via `HashSet`.
- `regen` already fetches MCP + external snapshots from `--repository-url` (`crates/cargo-agent-platform/src/commands/regen.rs`, default `http://127.0.0.1:18080/repository`). Adding a static-tool fetch is **symmetric** with what exists, not a new pattern.
- Repository read routes are embedded **in the runner process**, mounted at `/repository`, with **no auth** (`crates/baml-rt-api/src/router.rs:675`); only mutation routes get `auth_layer`. A new GET read route inherits no auth — matches existing reads.

### Finding 1 — runner is already slim (design works as intended)

> **⚠️ CORRECTED 2026-06-19 — this premise was FALSE in practice.** See [Runner Slimming Fixes](#runner-slimming-fixes-2026-06-19). The runner's own feature gates were dead: `baml-rt-api` (always linked by the runner) declared the HTTP tool crates **non-optional** and force-enabled `baml-rt-builder/http-tools`, and `baml-tools-calculator` was a non-optional dep of the runner + builder. A default runner therefore shipped **18 tools** (clickup/github/notion/slack/slack-notify/crm/email/calculate + system + claude), not the slim set. The endpoint was honest — it reported the fat reality. Both leaks are now fixed; the slim runner is real only after those changes.

`crates/baml-agent-runner/Cargo.toml:24-33` + `main.rs:54-68`: runner always links only `baml-tools-system` + `baml-tools-calculator`; clickup/github/notion/slack/slack-notify/grafana-alerts/security-eval are **feature-gated**. Runner does **not** call `force_link_all_tools!()`. So an endpoint that does `InventoryCatalog::new()` in-process returns the true slim-runner tool set. This is exactly the "slim runner reality" the plan wants — confirmed feasible.

### Finding 2 — serializable DTO already exists; don't invent one (revises Phase 1)

`ToolFunctionMetadata` (`crates/baml-rt-tools/src/tools.rs:919`) holds **no function pointers** — schemas are already materialized as `serde_json::Value`; invokers live separately in `ToolProvider`/inventory. The Phase 1 worry "avoid exposing invoker/runtime function pointers" is **moot**.

`ToolFunctionMetadataExport` **already exists** (`tools.rs:1242`): `#[derive(Serialize, Deserialize)]` + `From<&ToolFunctionMetadata>`. **Reuse/extend it** rather than authoring a new DTO from scratch.

BUT the existing Export is **incomplete for typegen** — it drops fields the builder reads:

| Field                                                       | Consumed by typegen?                                                        | Evidence                                                                                      | In Export today?  |
| ----------------------------------------------------------- | --------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- | ----------------- |
| `session_policy`                                            | YES — written into generated tool-card BAML                                 | `tool_interfaces.rs:163,173`                                                                  | **NO — must add** |
| `coordination_baml`                                         | YES — gathered into prelude (`_baml_runtime.baml`)                          | `codegen_pipeline.rs:155` → `gather_coordination_fragments` → `session_coordination.rs:85,89` | **NO — must add** |
| `name, class_name, description`                             | YES                                                                         | `tool_interfaces.rs`, `session_from_ir/catalog.rs`                                            | yes               |
| `open_input_schema, input_schema, output_schema`            | YES                                                                         | `tool_interfaces.rs:105-126`                                                                  | yes               |
| `open_input_type, input_type, output_type` (`ToolTypeSpec`) | YES                                                                         | `tool_interfaces.rs:74-130`                                                                   | yes               |
| `baml_decl, extra_ts_decls`                                 | YES (TS/BAML decl path)                                                     | `tool_interfaces.rs:74,101`                                                                   | yes               |
| `tags`                                                      | YES — tool card                                                             | `tool_interfaces.rs:181`                                                                      | yes               |
| `access`                                                    | manifest/allowlist validation                                               | —                                                                                             | yes               |
| `config / config_bundle`                                    | **NO for typegen** — only runner runtime config (`config_handlers.rs:544+`) | —                                                                                             | partial           |

**Action for Phase 1:**

1. Add `#[derive(Serialize, Deserialize)]` to `SessionPolicy` (`tools.rs:908`) and to any of its nested types lacking it.
2. Extend `ToolFunctionMetadataExport` (or wrap it in the versioned `StaticToolMetadata`) to include `session_policy` and `coordination_baml`. Include `config` + `config_bundle` too (cheap — both already `Serialize`) so the same DTO can later serve the runner's config API faithfully, even though typegen doesn't need them.
3. Build `From<&ToolFunctionMetadata> → DTO → ToolFunctionMetadata` round-trip and assert it preserves every field listed above.

### Finding 3 — `ToolCatalog` has 3 methods, not 2 (revises Phase 1/4)

`crates/baml-rt-tools/src/tool_catalog.rs:27-36`: `by_name`, `iter`, **and `bundle_config`**. Plan's `StaticToolSnapshotCatalog` sketch omits `bundle_config`. For the CLI typegen path `bundle_config` can return `None` (callers are runner-side only), but the impl must still provide it — back it by `config_bundle` if the DTO carries config, else `None` with a comment that config resolution is runner-local.

### Finding 4 — endpoint cannot hang off `RepositoryService` (revises Phase 2)

`RepositoryService::new(...)` (`crates/baml-agent-runner/src/main.rs:255`) is constructed from stores only (blob/metadata/lineage/search/mcp/external). It has **no inventory**. So the handler cannot read static tools from the service. Two options:

- **(Recommended) Inject a static snapshot at startup.** In runner `main.rs`, build `InventoryCatalog::new()` once, convert to `StaticToolCatalogResponse`, and pass it into `RepositoryService` (new field / setter). The existing `repository_read_router` then serves it. Keeps the repository as the single read surface.
- Add the route at the `baml-rt-api` layer where the runner process has inventory directly. Works, but splits the repository read surface across two crates.

Either way the route must be served by the **runner** process, never a detached repository — otherwise the inventory is empty and the bug just moves.

### Finding 5 — default-fetch is a dev-ergonomics regression (add to Risks)

Today a **pure-static** agent (no MCP, no external tools) regens fully **offline** — `InventoryCatalog::new()` is local and no snapshot HTTP is issued. After flipping the default to fetch-from-runner, those agents will **require a running runner**. Can't auto-detect "no static tools needed" without first having the catalog. Mitigations: keep the practical-interim all-tools CLI build until `doctor` lands; document `--use-local-inventory` loudly; consider only attempting the static fetch when the manifest references any non-`system`/`calculator` tool (best-effort heuristic, not a guarantee).

### Finding 6 — Phase 7 audit is too narrow

Removing `baml-tool-links` affects every CLI command that touches `InventoryCatalog`, not just `regen`. Before removal: `rg -n "InventoryCatalog|force_link_all_tools" crates/cargo-agent-platform` and migrate each consumer (scaffold/validate/list-tools/etc.) to the fetched catalog or gate it behind the dev feature. Otherwise those commands break silently.

#### Phase 7 audit results (2026-06-23)

Command used:

```bash
rg -n "InventoryCatalog|force_link_all_tools|baml_tool_links|load_cli_tools|tool_catalog" \
  crates/cargo-agent-platform/src crates/cargo-agent-platform/Cargo.toml
```

Remaining release-blocking consumers before `baml-tool-links` can be gated/removed:

| Area | Files | Current dependency | Break/change if links removed | Required action |
| --- | --- | --- | --- | --- |
| CLI startup | `src/main.rs`, `Cargo.toml` | unconditional `baml-tool-links` dep + `baml_tool_links::force_link_all_tools!()`; crate-level `#![allow(unexpected_cfgs)]` only exists for macro | default CLI no longer force-links all static tools; local `InventoryCatalog::new()` consumers shrink to whatever CLI itself links | gate dep + macro behind `local-static-inventory`; remove `#![allow(unexpected_cfgs)]` from default build after all remaining consumers migrate/dev-gate |
| Local picker catalog | `src/tool_catalog.rs` | **MIGRATED 2026-06-23:** no local `InventoryCatalog`; repository source reads `/repository/tools`; cache source reads unified snapshot cache | no local inventory blocker found | keep as migrated reference path; no default local fallback |
| Interactive new-agent tools | `src/interactive.rs:321,394` | **MIGRATED 2026-06-23:** `--repository-url` enables picker via runner catalog; `--snapshot-cache` enables picker via cache; repository wins when both are passed | no local inventory blocker found; no-source interactive flow skips picker with clear note | keep manual non-interactive `--tools`; no default local fallback |
| Interactive subscriptions | `src/interactive.rs:495` | **MIGRATED 2026-06-23:** source-kind prompt loads runner/cache static catalog for tool-declared `event_sources`; no-source path only shows common compatibility/system sources | no local inventory blocker found | keep static catalog path because `/repository/tools` summary omits `event_sources` |
| `list-event-sources` | `src/commands/list_event_sources.rs` | **MIGRATED 2026-06-23:** `--repository-url` fetches `/static-tools/snapshots`; `--snapshot-cache` reads `static-tools/catalog.json`; collector is generic over `ToolCatalog` | no local inventory blocker found | keep as migrated reference path; no default local fallback |
| `doctor` | `src/commands/doctor.rs` | static checks assume `baml-tool-links`; catalog checks use local `InventoryCatalog::new()` | doctor remains old-world static-link validator; missing-tool results compare manifests against CLI inventory instead of runner catalog | split doctor into runner-catalog doctor (default: `/static-tools/snapshots`, `/tools`, cache shape, manifest-vs-runner) and static-link doctor (dev-only, checks `baml-tool-links`, macro, feature coverage, local inventory) |
| `new-static-tool` patchers | `src/commands/new_static_tool.rs`, `src/patchers/tool_links_*` | intentionally edits `crates/baml-tool-links` and `force_link_all_tools!` | release CLI still carries internal workspace mutation command, but command does not require linked dependency at compile time | mark dev/internal; optionally feature-gate command and patchers with `local-static-inventory` or `static-tool-dev` |
| `new-agent` non-interactive | `src/commands/new_agent.rs` | uses `canonicalize_tool_ids()` only, no inventory lookup | no direct link-time dependency; canonicalization remains string-only | no blocker; validation against actual runner catalog should happen in regen/build/doctor, not here unless new flags added |
| `list-tools` / `export-snapshot-cache` / `regen` | `src/commands/list_tools.rs`, `export_snapshot_cache.rs`, `regen.rs` | already runner/cache-backed for static catalog path | no local inventory blocker found | keep as migrated reference paths |

Conclusion: do **not** remove default `baml-tool-links` yet. Minimum safe next PR: split `doctor` into runner-catalog default and dev-only static-link checks. After that, gate startup macro and dep.

### Finding 7 — internal coordination travels a second channel; inline it (no new machinery)

`gather_coordination_fragments` (`session_coordination.rs:85`) reads coordination per-tool from **two** sources: `meta.coordination_baml` (the bundle field) **and** `render_inventory_fragment(tool_id)` (the `SessionCoordinationProvider` inventory). Internal static tools leave `meta.coordination_baml = None` and ship their fragment **only** via the inventory provider (e.g. `crates/tools/claude/src/session_coordination.rs`); no `with_coordination_baml` setter exists. Only the **external** path sets the field (`external_tools/metadata.rs:416`, `snapshot.rs:359`).

Consequence: `ToolFunctionMetadataExport::from(metadata)` copies `None` for internal tools, so a serialized catalog **drops their coordination**. A thin CLI (no inventory linked) then has neither source → the `Choose<X>Action` flow is silently missing from `_baml_runtime.baml`. The struct round-trip cannot catch this — the data lives outside the struct.

**Fix is minimal and already-precedented — do NOT invent new machinery.** External tools already solve the identical "fragment must travel over the wire" problem by *inlining* it into `coordination_baml` at snapshot time (`snapshot.rs:359` `inline_coordination_baml`). Mirror that for static: the runner-side projection (`from_inventory`/`from_catalog`) calls the existing `render_inventory_fragment(tool_id)` and writes the result into the existing `coordination_baml` field. No new DTO, no new field, no new field-resolution path — one resolution step reusing existing functions. Thin CLI then finds it in the bundle slot; its inventory slot is `None`, so the "both sources" guard never trips.

### Sequencing (gate first)

1. **Phase 1 DTO extension + round-trip + parity test is the gate.** Parity test = regen output (generated TS **and** `_baml_runtime.baml` prelude) from a fetched/file catalog is byte-identical to local-inventory output, for at least one coordination tool and one config-bundle tool. If parity fails, the rest of the plan is moot — do this before any endpoint work.
2. Practical interim (ship all-tools CLI + add `doctor`) in parallel — decouples release from this effort.
3. **Do Phase 7 (remove links) last**, only after `doctor` + several real agents regen clean from the endpoint in CI. It is the ergonomic cliff.

### Nits

- Endpoint name: existing external snapshots are at `/repository/external-tools/snapshots`. For symmetry use `/repository/static-tools/snapshots` (or `/repository/static-tools`).
- DTO: keep `schema_version`; add runner `git_sha` **and the enabled-features list** so `doctor` can explain _why_ a tool is absent (feature off vs not built) rather than just "missing".

### Scope decisions (2026-06-18)

- **No source enums.** `StaticToolSource` (Phase 4) and `StaticToolSourceConfig` (Phase 5) are removed from the plan. External/MCP have no such enums — they branch inline on existing fields and `composite.add(...)`. Static mirrors that exactly. Add an enum later only if a real third caller appears.
- **Config deferred to a later iteration.** No config handling / runner config API for internal static tools now. The `config` / `config_bundle` fields already ride along in `ToolFunctionMetadataExport` (free, keeps the round-trip lossless and feeds the `ToolCatalog::bundle_config` trait default) — but building config **resolution** for static tools is out of scope for this pass. Typegen does not need config; ship typegen parity first, revisit config as its own iteration.

## Runner Slimming Fixes (2026-06-19)

While wiring the endpoint we discovered a default runner ships **18** static tools, not a slim set — directly invalidating [Finding 1](#finding-1--runner-is-already-slim-design-works-as-intended). Root cause: static tools register via link-time `inventory`, so *any* tool crate compiled into the binary auto-registers; the runner's own `optional`/feature gates only control its **direct** edges, not transitive ones. Two leak sources, both now fixed. The mechanism for both: make the crate `optional`, feature-gate the `use … as _;` force-links with `#[cfg(feature = …)]`, and forward the feature down the dependency chain. Tests keep the tools via `[dev-dependencies]`, so CI needs no feature changes.

### Fix A — `dev-tools` feature gates the demo/test tools (calculator)

`calculator` (`support/calculate`) is a demo tool with no production consumer (only fixtures/tests use it). Scoped to calculator only per decision; `security-eval` (crm/email) and `internal-dev` are the same class but left for later.

- `crates/baml-agent-runner/Cargo.toml`: `baml-tools-calculator` → `optional = true`; new feature `dev-tools = ["dep:baml-tools-calculator"]` (NOT in `http-tools`).
- `crates/baml-rt-builder/Cargo.toml`: same — `optional` + `dev-tools` feature.
- Force-links gated with `#[cfg(feature = "dev-tools")]`: `baml-agent-runner/src/main.rs`, `baml-rt-builder/src/baml-agent-builder.rs`, and **both** sites in `baml-rt-builder/src/builder/baml_gen/tool_interfaces.rs` (the `use … as _;` and the real `let _ = baml_tools_calculator::support_calculate_metadata;`).
- No justfile/CI change: tests reach calculator via `test-support` (a dev-dep that links it), so `InventoryCatalog::new()` in test binaries still sees `support/calculate`.
- Effect: default `cargo build -p baml-agent-runner` and the production `Dockerfile` (`--features http-tools`, no `dev-tools`) drop calculator. `--all-features` dev builds + `regen-fixtures` keep it.
- **Verified live**: default runner returned 17 tools, `has_calculate: false`.

### Fix B — `baml-rt-api` no longer force-links the HTTP tools (the real leak)

`baml-rt-api` is always linked by the runner. It declared clickup/github/notion/security-eval/slack as **non-optional** deps (`Cargo.toml:23-28`), force-enabled `baml-rt-builder = { features = ["http-tools"] }` (`:18`, dragging in slack-notify + grafana too), and force-linked them via `use … as _;` in `event_console/mod.rs` + `repository_publish.rs` — so every runner carried the full HTTP set + crm/email regardless of its own flags. The force-linking exists on purpose (so runner-side `POST /repository/publish` typegen sees all tools), so the fix preserves it *behind features*:

- `crates/baml-rt-api/Cargo.toml`: dropped forced `http-tools` from the builder dep; clickup/github/notion/security-eval/slack → `optional = true`; added `[features]` where each tool pulls `dep:baml-tools-<tool>` **and** forwards to `baml-rt-builder/<tool>` (`slack-notify`/`grafana-alerts`/`memory` have no direct api dep, so forward to builder only); added `http-tools` umbrella; added clickup/github/slack to `[dev-dependencies]` for the `event_console::registry` unit test (which deserializes their batch types).
- Force-links gated `#[cfg(feature = …)]`: `event_console/mod.rs` (clickup/github/slack; `system` stays always-on), `repository_publish.rs` (notion/security-eval).
- `crates/baml-agent-runner/Cargo.toml`: every tool feature now also forwards `→ baml-rt-api/<tool>`, so a runner built with a tool feature propagates runner → api → builder. Without this, a featured runner would still get a slim api and lose the tool at publish time.
- Propagation chain: `runner --features http-tools` → `baml-rt-api/http-tools` → `baml-rt-builder/http-tools`. Default (no features) → everything slim. Only `baml-agent-runner` consumes api, so this is the only forward needed.
- **Verified** (`cargo check`, exit 0): api default (slim) / api `http-tools` / api `--tests` (registry test via dev-deps) / runner default (slim) / runner `http-tools` (tools propagate). Live default-runner curl was not re-run.

### Remaining (deliberately out of scope)

- `security-eval` (crm/email) still ships under `http-tools` — only gated out of a no-feature build. Fold into `dev-tools` later if it should never be in production.
- The deploy patcher (`cargo-agent-platform/src/patchers/runner_main_rs.rs`) and `baml-tool-links` still treat calculator/HTTP tools as always-present; the CLI/local-inventory path is unchanged.
- This makes the endpoint's "slim runner" premise real, but a default runner can no longer typegen an agent using an un-enabled tool — which is the intended behavior and exactly why the static-tool catalog endpoint (the rest of this plan) matters: the CLI must fetch the runner's real catalog, not assume.

## Current Problem

Static Rust tools are registered through Rust `inventory`. Metadata exists only inside binaries that link tool crates.

Today:

- `cargo-agent-platform` depends on `baml-tool-links` with `features = ["all-tools"]`.
- CLI calls `baml_tool_links::force_link_all_tools!()`.
- `RuntimeTypeGenerator` and builder catalogs use local `InventoryCatalog::new()`.
- MCP/external tools can come from repository/cache snapshots, but static Rust tools still come from current process inventory.

This means downloaded CLI may:

- miss private/internal static tools;
- include tools target runner does not have;
- produce schema universe different from deployment runner;
- require broad/heavy release binary just for typegen.

## Target Design

Runner/repository exposes static compiled tool metadata from its own linked inventory.

Suggested endpoint:

```http
GET /repository/static-tools/schemas
```

Alternative acceptable names:

```http
GET /repository/tools/static
GET /tools/schemas
GET /repository/tool-catalog/static
```

Response should include all static tools linked into target runner binary as serialized metadata needed by builder/typegen.

Sketch:

```json
{
  "schema_version": "static-tool-catalog.v1",
  "runner_version": "0.1.0",
  "git_sha": "...",
  "tools": [
    {
      "name": "support/slack_notify",
      "description": "...",
      "access": "write",
      "input_schema": { "type": "object" },
      "output_schema": { "type": "object" },
      "schemas": { "events": [] },
      "execution": { "kind": "static" }
    }
  ]
}
```

Exact shape should reuse existing `ToolMetadata` serialization if stable enough; otherwise define versioned DTO.

## Catalog Composition After Change

For `regen` and repository-side builds:

1. Static tool catalog from runner/repository endpoint.
2. Approved external-tool snapshots from repository or offline cache.
3. Approved MCP snapshots from repository or offline cache.
4. Collision validation across all sources.
5. Generate BAML prelude and TypeScript declarations.

Local `InventoryCatalog::new()` becomes dev fallback only, not default release behavior.

## CLI UX

Preferred default:

```bash
cargo agent-platform regen my-agent --repository-url http://runner:18080/repository
```

Behavior:

- Fetch static tool catalog from `--repository-url`.
- Fetch MCP/external snapshots from same repository URL.
- Generate types from fetched catalog.
- If target static tool missing, fail clearly:

```text
runner static tool catalog does not include support/slack_notify
Target runner may be built without that tool. Rebuild runner with required tool or choose compatible runner.
```

Offline mode uses one cache root for every tool catalog:

```text
.baml-snapshot-cache/
  static-tools/
    catalog.json
  external-tools/
    snapshots/...
  mcp/
    servers/...
```

```bash
cargo agent-platform regen my-agent \
  --snapshot-cache .baml-snapshot-cache
```

Behavior:

- `--snapshot-cache` is all-or-nothing offline catalog mode.
- Static tools load from `.baml-snapshot-cache/static-tools/catalog.json`.
- MCP/external load from their existing cache directories.
- Registry is not contacted.
- Missing static catalog file is a hard error with an export-compatible-runner hint.
- No `--static-tool-catalog` single-file override and no local-inventory CLI fallback for now; avoid per-source mixing flags.

## Implementation Plan

### Phase 1: Define Static Tool Catalog DTO

Create versioned DTO for static tool metadata.

Likely location:

- `crates/baml-rt-tools/src/static_tool_catalog.rs`
- or `crates/baml-rt-repository/src/http.rs` for HTTP response DTO plus reusable conversion in tools crate.

Requirements:

- Serialize all data required by typegen.
- Avoid exposing invoker/runtime function pointers.
- Include catalog schema version.
- Include optional runner/build metadata: app version, git SHA, features if available.
- Add conversion from `InventoryCatalog::new()` to DTO.
- Add conversion from DTO back into a `ToolCatalog` implementation for builder/typegen.

Proposed types (built on the **existing** `ToolFunctionMetadataExport`, see Finding 2 — do not author the per-tool DTO from scratch):

```rust
// `StaticToolMetadata` = ToolFunctionMetadataExport EXTENDED with the two
// typegen-consumed fields it currently drops. Requires deriving
// Serialize/Deserialize on SessionPolicy (tools.rs:908).
pub struct StaticToolMetadata {
    // ... all current ToolFunctionMetadataExport fields ...
    pub session_policy: SessionPolicy,        // ADD — tool_interfaces.rs:163,173
    pub coordination_baml: Option<String>,    // ADD — codegen_pipeline.rs:155
    pub config: Option<ToolConfigMetadata>,   // rides along for round-trip; config handling DEFERRED (later iteration)
    pub config_bundle: Option<BundleName>,    // rides along for bundle_config() trait default; no config resolution this pass
}

pub struct StaticToolCatalogResponse {
    pub schema_version: String,           // "static-tool-catalog.v1"
    pub runner_version: Option<String>,
    pub git_sha: Option<String>,
    pub enabled_features: Vec<String>,    // ADD — lets `doctor` explain WHY a tool is absent
    pub tools: Vec<StaticToolMetadata>,
}

pub struct StaticToolSnapshotCatalog {
    tools: Vec<ToolFunctionMetadata>,     // reconstructed from StaticToolMetadata
}

// NOTE: ToolCatalog has THREE methods (tool_catalog.rs:27) — by_name, iter, bundle_config.
impl ToolCatalog for StaticToolSnapshotCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> { /* ... */ }
    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> { /* ... */ }
    fn bundle_config(&self, name: &BundleName) -> Option<&ToolFunctionMetadata> {
        // backed by config_bundle if carried; runner-side concern, fine to be best-effort here
    }
}
```

### Phase 2: Add Runner/Repository Endpoint

Expose static inventory through runner-hosted repository routes.

Likely files:

- `crates/baml-rt-repository/src/http.rs`
- `crates/baml-rt-repository/src/handlers.rs`
- `crates/baml-rt-repository/src/router.rs`
- or if handler needs runner inventory directly, add route in `crates/baml-rt-api/src/router.rs` / runner host layer.

Endpoint:

```http
GET /repository/static-tools/schemas
```

Handler logic:

1. Construct `InventoryCatalog::new()` in runner process.
2. Convert to `StaticToolCatalogResponse`.
3. Return JSON.

Important: endpoint must run in runner binary, not generic repository service detached from runner inventory, otherwise same problem moves elsewhere.

### Phase 3: Add HTTP Client Fetcher

Add client helper in builder or tools crate.

Likely location:

- `crates/baml-rt-builder/src/static_tool_registry.rs`
- or `crates/baml-rt-tools/src/static_tool_registry.rs`

Behavior:

- GET `{repository_url}/static-tools/schemas`.
- Validate `schema_version`.
- Decode catalog.
- Build `StaticToolSnapshotCatalog`.
- Surface useful errors for 404, bad JSON, unsupported schema version.

### Phase 4: Refactor Builder Catalog Composition

`build_builder_catalog_with_roots` hardcodes `InventoryCatalog::new()` at `metadata_catalog.rs:43`. That is the **only** swap point.

**Decision (2026-06-18): mirror external tools — NO `StaticToolSource` enum, NO `build_builder_catalog_with_sources`.** External/MCP do not introduce a source enum; they branch inline on the catalog's existing roots and `composite.add(...)`. Static does the same. The enum machinery is dropped from this plan.

Change: thread an optional pre-resolved static catalog into the composition and use it in place of `InventoryCatalog::new()` when present. When absent, fall back to `InventoryCatalog::new()` — this fallback **is** the `--use-local-inventory` / dev path, free.

```rust
// Replace the hardcoded inventory at metadata_catalog.rs:43 with the injected
// static catalog when present; else keep InventoryCatalog::new().
let inventory: Box<dyn ToolCatalog> = match static_catalog {
    Some(c) => Box::new(c),          // StaticToolSnapshotCatalog
    None => Box::new(InventoryCatalog::new()),
};
```

Collision rules unchanged (already enforced inside `build_builder_catalog_with_roots`):

- static vs external: error.
- static vs MCP: error.
- external vs MCP: error.

### Phase 5: Update `RuntimeTypeGenerator`

**Decision (2026-06-18): reuse the existing fields — NO `StaticToolSourceConfig` enum, NO new constructors.** `RuntimeTypeGenerator` already holds `registry_service` / `registry_url` / `snapshot_cache_root`, and external/MCP branch on exactly these three. Static mirrors that — the struct gains **no new field**.

```rust
pub struct RuntimeTypeGenerator {
    registry_service: Option<Arc<RepositoryService>>,
    registry_url: Option<String>,
    snapshot_cache_root: Option<PathBuf>,
    // no new field
}
```

Add `prepare_static_tool_catalog()` alongside `prepare_external_tool_snapshots()`, same branch order on the existing fields:

1. `registry_service` present → read injected `StaticToolCatalogResponse` in-process and write it into `build_dir/static-tools/catalog.json`.
2. `snapshot_cache_root` present → read `snapshot_cache_root/static-tools/catalog.json` and copy it into `build_dir/static-tools/catalog.json` (offline all-catalog mode).
3. `registry_url` present → HTTP GET `/repository/static-tools/snapshots` and write it into `build_dir/static-tools/catalog.json`.
4. none → no static catalog file; local `InventoryCatalog::new()` remains only for internal/dev callers that construct a generator with no registry/cache.

Workspace materialization then loads `build_dir/static-tools/catalog.json`, mirroring MCP/external build-dir cache flow, and injects it into Phase 4 catalog composition.

`generate()` flow:

1. Prepare/fetch/copy static catalog into `build_dir/static-tools/catalog.json`.
2. Prepare MCP snapshots into `build_dir/mcp`.
3. Prepare external snapshots into `build_dir/external-tools`.
4. Materialize workspace using composed catalog.

### Phase 6: Update `cargo-agent-platform regen`

Rules:

- Default `regen` fetches all catalogs from `--repository-url`.
- `--snapshot-cache DIR` is the only offline switch and applies to all catalogs:
  - static: `DIR/static-tools/catalog.json`
  - external: existing external snapshot cache
  - MCP: existing MCP snapshot cache
- No `--static-tool-catalog` single-file flag.
- No `--use-local-inventory` CLI flag for now; avoid per-source mixing semantics.
- Endpoint/cache failures are hard errors. No silent fallback to local inventory.

Avoid silent fallback from endpoint/cache failure to local inventory. Silent fallback recreates stale/wrong schema bug.

### Phase 7: Remove CLI Static Tool Linking From Release Path

After `regen` no longer needs local static inventory by default:

- Remove `baml-tool-links` dependency from `crates/cargo-agent-platform/Cargo.toml`, or gate behind dev feature.
- Remove unconditional `baml_tool_links::force_link_all_tools!()` from `main.rs`.
- Keep local-inventory fallback behind feature flag if needed:

```toml
[features]
local-inventory = ["baml-tool-links/all-tools"]
```

Release binary should be thin by default.

### Phase 8: Add Doctor/Compare Command

Add operator diagnostics:

```bash
cargo agent-platform doctor --repository-url http://runner:18080/repository
```

Checks:

- static catalog endpoint reachable;
- runner version/build metadata visible;
- list static tools count;
- if local inventory feature enabled, compare local vs runner inventories;
- warn about missing/extra tools.

Optional command:

```bash
cargo agent-platform list-tools --repository-url ...
```

Should list runner catalog by default, not local inventory.

### Phase 9: Tests

Add tests for:

- static catalog DTO round-trip;
- endpoint returns linked inventory metadata;
- fetched static catalog implements `ToolCatalog` correctly;
- regen fails when manifest references static tool absent from fetched catalog;
- regen succeeds when fetched catalog contains tool even CLI local inventory does not;
- no silent fallback to local inventory on endpoint failure;
- external/MCP snapshot collision with static catalog;
- offline `--static-tool-catalog` mode.

Useful fixture strategy:

- Build tiny fake static catalog JSON with one tool.
- Agent manifest references that tool.
- Run typegen using catalog JSON without linking actual tool crate.
- Assert generated TypeScript includes tool types.

### Phase 10: Docs and Migration

Update docs/runbooks:

- regen now targets runner/repository catalog;
- release CLI no longer bundles all static tool crates;
- private/internal tool support requires runner endpoint access;
- offline CI uses exported static catalog + snapshot cache;
- dev fallback documented as unsafe for deploy parity.

Migration note:

- Existing `cargo run -p cargo-agent-platform -- regen` may need `--use-local-inventory` in pure workspace dev without runner running.
- CI should start runner/repository or provide static catalog file.

## Risks / Open Decisions

### Endpoint ownership

If repository can run standalone without runner inventory, endpoint may not know static tools. For correctness, static tool endpoint should be served by runner process or be explicitly populated by runner at startup.

### DTO stability

Directly serializing internal `ToolMetadata` may be quick but fragile. Versioned DTO gives safer long-term contract.

### Bootstrap loop

Repository publish/build route may itself need typegen. If route runs inside runner process, local inventory there is valid because runner is source of truth. CLI still should fetch from runner.

### Auth

Static tool schemas likely safe read-only metadata, same class as repository read routes. If private tool names/schemas are sensitive, endpoint needs same auth policy as private repository reads.

### Slim runner mismatch

This design intentionally exposes slim runner reality. Agents using absent static tools should fail before deploy/typegen with explicit missing-tool message.

## Definition of Done

- Downloaded thin CLI can regen agent using private static tool metadata fetched from runner, without linking private tool crate.
- CLI no longer needs `baml-tool-links/all-tools` for normal regen.
- Missing static tool error names runner/catalog as source of truth.
- Offline regen possible with exported static catalog file.
- Tests prove no silent fallback to local inventory.

## Subtasks Checklist

> **Tests are deferred until after Phase 4/5.** All test subtasks below (round-trip, parity gate, unit, integration, collision) require the catalog→builder→typegen path to exist before there is anything to assert against. Do not write them until Phase 4/5 wiring lands; then the parity gate (Finding 7) runs first and gates Phase 7.

- [x] Choose final endpoint path: **`GET /repository/static-tools/snapshots`** (symmetry with `/repository/external-tools/snapshots`).
- [x] Derive `Serialize`/`Deserialize` on `SessionPolicy` (`tools.rs:908`). Nested types already serde-ready: `BundleName` (manual impls `tools.rs:516,525`), `ToolConfigMetadata`, `ToolTypeSpec`, `ToolProjectionSemantics`, `EventSourceKind`.
- [x] Extend `ToolFunctionMetadataExport` (`tools.rs:1242`) with `session_policy`, `coordination_baml`, `config_bundle` (`config` already present) + updated `From<&ToolFunctionMetadata>`. New fields `#[serde(default)]` for back-compat.
- [x] ~~Add `enabled_features` to `StaticToolCatalogResponse`.~~ **DROPPED (2026-06-18):** leaks the build's gated-capability vocabulary to an unauthenticated endpoint without helping an authorized caller (who already has the full `tools[]`). Replaced by `runner_version` + `git_sha` advisory fields; missing-tool error names the runner build instead. Real exposure decision (auth on whole endpoint serving private schemas) deferred to Phase 2.
- [ ] Add DTO round-trip test asserting ALL typegen-consumed fields survive (see Finding 2 table). _(Deferred — initial tests removed as low-value at this stage; revisit alongside the parity gate.)_
- [x] Implement conversion from `InventoryCatalog` to DTO — `StaticToolCatalogResponse::from_inventory` / `from_catalog`.
- [x] Implement `StaticToolSnapshotCatalog` satisfying ALL THREE `ToolCatalog` methods (incl. `bundle_config` via trait default — `config_bundle` reconstructed losslessly). New module `crates/baml-rt-tools/src/static_tool_catalog.rs`; `missing_tool_error()` emits the version/sha-aware message.
- [ ] **Parity gate test**: fetched/file-catalog regen output (TS + `_baml_runtime.baml` prelude) byte-identical to local-inventory output, for a coordination tool + a config-bundle tool. Run before endpoint work.
- [x] **Projection inlines internal coordination** (Finding 7): runner-side `from_inventory`/`from_catalog` calls existing `render_inventory_fragment(tool_id)` and writes it into the existing `coordination_baml` field when `None`. Mirrors external-tool `inline_coordination_baml` — no new field/DTO. Without this, internal-tool coordination is dropped on a thin CLI.
- [x] Inject `StaticToolCatalogResponse` (from `InventoryCatalog::new()`) into `RepositoryService` at runner startup (`baml-agent-runner/main.rs:255`) — service has no inventory today (Finding 4). Builder method `RepositoryService::with_static_tool_catalog(...)` + getter `static_tool_catalog()`; `new(...)` signature unchanged (defaults `None`) so existing callers/tests untouched. Runner stamps `runner_version = CARGO_PKG_VERSION`, `git_sha = option_env!("GIT_SHA")`.
- [x] Add runner-hosted endpoint returning current process static inventory. `GET /repository/static-tools/snapshots` → `handlers::get_static_tool_catalog` (returns injected `StaticToolCatalogResponse`, 404 if absent). Wired into both `repository_read_router` and `repository_router_with_publish`; metric label `repository_static_tool_catalog` (`baml-rt-api/router.rs`). No auth (read route, matches existing reads). **Verified live**: slim debug runner returned 18 tools, schema `static-tool-catalog.v1`, `runner_version 0.1.1`; internal `claude/dev` carried inlined `coordination_baml` + `session_policy` + schemas (Finding 7 projection confirmed end-to-end).
- [x] Add HTTP client fetcher for static tool catalog. `baml-rt-builder/src/static_tool_registry.rs::fetch_static_tool_catalog` — GET `/static-tools/snapshots`, decode bare `StaticToolCatalogResponse`, project via `from_response`; 404 → loud error (no silent empty catalog). Mirrors `external_tool_registry.rs`.
- [x] Add file loader for offline static tool catalog JSON. Same module, `load_static_tool_catalog_from_file` — read JSON, decode, project. Both registered in `lib.rs`. `cargo check -p baml-rt-builder` passes.
- [x] Refactor builder catalog composition to accept injected static source. `build_builder_catalog_with_static_catalog(...)` now accepts an explicit static `ToolCatalog`; `None` preserves local inventory only for current/explicit dev callers. Duplicate tool names remain a hard invariant across static/external/MCP.
- [x] Update `RuntimeTypeGenerator` to fetch/use static catalog by default with registry URL. It now resolves an embedded `RepositoryService` catalog, `--snapshot-cache` catalog, or `/static-tools/snapshots`, writes/copies the response into `build_dir/static-tools/catalog.json`, and workspace materialization loads that file like MCP/external caches. No registry/cache still preserves local inventory behavior only for internal/dev callers.
- [x] Use unified `--snapshot-cache` for offline static catalogs. Static catalog lives at `DIR/static-tools/catalog.json`; no `regen --static-tool-catalog <path>` single-file flag remains.
- [x] Drop `regen --use-local-inventory` from current CLI plan. Local inventory remains an internal/dev no-registry/no-cache behavior, not an operator flag, to avoid per-source mixing semantics.
- [x] Ensure endpoint/cache failure does not silently fall back to local inventory. Default registry fetch and offline cache load both hard-error before workspace materialization.
- [x] Update missing-tool errors to mention runner static catalog. Generic manifest resolution now reports: if missing name is static, target runner static catalog may not include it; if MCP/external, import/approve the required registry snapshot. Removed unused runner-specific helper fields/methods from `StaticToolSnapshotCatalog`.
- [x] Add unified snapshot-cache export command. `cargo agent-platform export-snapshot-cache --repository-url ... --output DIR` exports all static tools to `DIR/static-tools/catalog.json`, all approved external snapshots to `DIR/external-tools/...`, and latest approved MCP server snapshots to `DIR/mcp/...`.
- [x] Update `list-tools` to query runner catalog. Added unified `GET /repository/tools[?source=static|external|mcp]` inventory endpoint (source omitted returns all approved/visible tools). CLI `list-tools` now queries runner/repository by default and supports `--snapshot-cache DIR` to list from unified offline cache.
- [x] Audit ALL `InventoryCatalog`/`force_link_all_tools` consumers in `cargo-agent-platform` (not just regen) before removing the dep (Finding 6). Findings captured under “Phase 7 audit results (2026-06-23)”; blockers remain in `doctor`, startup macro/dep, and dev patchers.
- [x] Migrate `list-event-sources` off default local `InventoryCatalog`: added `--repository-url`, `--snapshot-cache`, runner fetch, cache load, and generic `ToolCatalog` collector.
- [x] Migrate interactive new-agent picker/subscriptions off default local `InventoryCatalog`: picker exists only with `--repository-url` or `--snapshot-cache` (repository wins); no-source interactive flow skips picker; subscription prompt loads runner/cache static catalog for tool-declared `event_sources`.
- [ ] Gate or remove `baml-tool-links` dependency from release CLI.
- [ ] Remove unconditional `force_link_all_tools!()` from release CLI startup.
- [ ] Add unit tests for DTO round-trip and catalog implementation.
- [ ] Add integration test: regen succeeds from fetched catalog without local tool link.
- [ ] Add integration test: missing static tool in fetched catalog fails clearly.
- [ ] Add collision tests across static/external/MCP catalogs.
- [ ] Update regen docs/runbooks and release install docs.
- [ ] Update TODO `TODO-aea950b2` with implementation result and close when complete.
