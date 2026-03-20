# Agentium OS SDK CLI — Implementation Plan

## 1. Executive Summary

Build a `cargo-agent-platform` CLI (invoked as `cargo agent-platform <subcommand>`) that automates scaffolding of new tools and agents, eliminating manual boilerplate while preserving the existing compile-time tool registration model. WASM-based external tools are deferred to a later phase.

The CLI is delivered as a new workspace crate (`crates/cargo-agent-platform`) and integrates with the existing builder, runner, and CI infrastructure.

---

## 2. Current Pain Points

| Pain | Root Cause |
|------|-----------|
| Creating a tool requires manually creating a crate, adding 6+ derives per type, writing `Cargo.toml`, updating workspace `Cargo.toml`, updating `baml-agent-runner/Cargo.toml` features, and adding `use` lines in 3 Rust source files | No scaffolding exists for tools |
| Creating an agent is easier (`bootstrap.rs` exists) but isn't exposed as a top-level CLI command | `run_bootstrap` is library code inside `baml-rt-builder`, invoked only by the builder binary interactively |
| CI doesn't validate that generated files (`generated_tools.baml`, `baml-runtime.d.ts`) are up to date after tool/agent changes | No diff-check step in `rust-ci.yml` |
| Adding a tool to the runner requires knowing 6 different files to edit | Linkage is manual and error-prone |
| Force-link `use ... as _;` lines are spread across 3 files with inconsistent patterns | No centralized force-link mechanism |

---

## 3. Deliverables

### 3.1 `cargo-agent-platform` CLI Binary

A workspace binary crate with the following subcommands:

```
cargo agent-platform new-tool <name> [--bundle support] [--with-client] [--access <level>] [--dry-run]
cargo agent-platform new-agent <name> [--tools <tool1,tool2,...>] [--template <template>] [--description <desc>]
cargo agent-platform regen
cargo agent-platform list-tools
cargo agent-platform list-agents
cargo agent-platform doctor [--ci] [--warn-missing-catalog]
```

### 3.2 `force_link_all_tools!()` Macro

A macro in a new leaf crate `crates/baml-tool-links` that centralizes tool force-linking, replacing the per-file manual `use ... as _;` lines. This crate exists solely to avoid a dependency cycle (`baml-rt-tools` cannot depend on tool crates because tool crates depend on `baml-rt-tools`).

### 3.3 CI Enhancements

New CI steps to validate generated code and scaffold integrity.

### 3.4 Templates

Reusable templates for common agent patterns.

---

## 4. `force_link_all_tools!()` — Centralized Force-Linking

### 4.1 Problem

Today, force-link `use ... as _;` lines are manually maintained across 3 files with inconsistent patterns:

| File | Unconditional links | Feature-gated links |
|------|--------------------|--------------------|
| `baml-agent-runner/src/main.rs` (lines 60-68) | `calculator` | `clickup`, `memory`, `notion`, `slack` |
| `baml-rt-builder/src/baml-agent-builder.rs` (lines 31-39) | `claude`, `calculator`, `system` | `clickup`, `notion`, `slack` |
| `baml-rt-builder/src/bin/regen_fixtures.rs` (lines 11-14) | `claude`, `system` | `slack` |

Adding a new tool requires patching all 3 files, and the pattern is already inconsistent (e.g. `memory` is in runner but not builder, `calculator` is unconditional in runner but feature-gated nowhere).

### 4.2 Solution

Create a new leaf crate `crates/baml-tool-links` whose sole purpose is hosting the `force_link_all_tools!()` macro and carrying all tool crates as optional dependencies.

**Why a separate crate?** Tool crates (clickup, notion, etc.) depend on `baml-rt-tools` for the `BamlTool` trait, `baml_tool` macro, bundle types, etc. If `baml-rt-tools` depended back on tool crates, that would be a circular dependency. `baml-tool-links` breaks the cycle — it's a pure leaf that depends on tool crates but nothing depends on it except the final binaries (runner, builder, CLI).

```
                        ┌──────────────┐
                        │ baml-rt-tools│  (trait, macro, bundles)
                        └──────┬───────┘
                               │ depended on by
              ┌────────────────┼────────────────┐
              ▼                ▼                 ▼
     baml-tools-clickup  baml-tools-notion  baml-tools-slack  ...
              │                │                 │
              │ depended on by (optional, feature-gated)
              ▼                ▼                 ▼
                    ┌──────────────────┐
                    │ baml-tool-links  │  (macro + optional deps)
                    └────────┬─────────┘
                             │ depended on by
              ┌──────────────┼──────────────┐
              ▼              ▼              ▼
         runner          builder          CLI
```

**Crate contents:**

```rust
// crates/baml-tool-links/src/lib.rs

/// Force-link all registered tool crates into the binary's inventory.
///
/// Call this macro once at the top level of any binary that needs tool
/// discovery (runner, builder, CLI). The `#[cfg(feature)]` gates match
/// the feature flags on this crate's Cargo.toml.
#[macro_export]
macro_rules! force_link_all_tools {
    () => {
        // Unconditional (always linked — core platform tools)
        use ::baml_rt_tools_claude as _;
        use ::baml_tools_system as _;
        use ::baml_tools_calculator as _;

        // Feature-gated (integration tools)
        #[cfg(feature = "clickup")]
        use ::baml_tools_clickup as _;
        #[cfg(feature = "memory")]
        use ::baml_tools_memory as _;
        #[cfg(feature = "notion")]
        use ::baml_tools_notion as _;
        #[cfg(feature = "slack")]
        use ::baml_tools_slack as _;
    };
}
```

```toml
# crates/baml-tool-links/Cargo.toml
[package]
name = "baml-tool-links"
version.workspace = true
edition.workspace = true

[dependencies]
# Unconditional (core platform tools)
baml-rt-tools-claude = { path = "../tools/claude" }
baml-tools-system    = { path = "../tools/system" }
baml-tools-calculator = { path = "../tools/calculator" }

# Feature-gated (integration tools)
baml-tools-clickup = { path = "../tools/clickup", optional = true }
baml-tools-memory  = { path = "../tools/memory", optional = true }
baml-tools-notion  = { path = "../tools/notion", optional = true }
baml-tools-slack   = { path = "../tools/slack", optional = true }

[features]
default = []
clickup = ["dep:baml-tools-clickup"]
memory  = ["dep:baml-tools-memory"]
notion  = ["dep:baml-tools-notion"]
slack   = ["dep:baml-tools-slack"]
http-tools = ["clickup", "notion", "slack"]
all-tools  = ["http-tools", "memory"]
```

Each binary replaces its manual `use ... as _;` lines with a single call:

```rust
baml_tool_links::force_link_all_tools!();
```

**Runner/Builder `Cargo.toml` changes:**
- Remove direct optional dependencies on individual tool crates
- Add single dependency: `baml-tool-links = { path = "../baml-tool-links" }` with feature forwarding
- Feature flags on runner/builder forward to `baml-tool-links` features:
  ```toml
  [features]
  clickup = ["baml-tool-links/clickup"]
  memory  = ["baml-tool-links/memory"]
  http-tools = ["baml-tool-links/http-tools"]
  ```

**Benefits:**
- Adding a new tool requires patching only `baml-tool-links/` (Cargo.toml + lib.rs) instead of 3 binary source files
- No circular dependency — `baml-tool-links` is a pure leaf crate
- Runner/builder feature flags forward cleanly to a single place
- `new-tool` command has a single patch target for force-linking
- Consistency is enforced by construction
- `doctor` only needs to verify the `baml-tool-links` crate is up to date

**Alternatives considered:**
- Putting the macro in `baml-rt-tools` — rejected, creates dependency cycle (tool crates depend on `baml-rt-tools`)
- A `build.rs` that scans features and generates `use` lines — rejected, too magical and harder to audit

### 4.3 `new-tool` patch target after macro adoption

With the macro in place, `new-tool` patches:

| File | Change |
|------|--------|
| `Cargo.toml` (workspace root) | Add `crates/tools/<name>` to `members` |
| `crates/baml-tool-links/Cargo.toml` | Add optional dep + feature + add to `all-tools` |
| `crates/baml-tool-links/src/lib.rs` | Add `#[cfg(feature = "<name>")] use ::baml_tools_<name> as _;` inside the macro |
| `crates/baml-agent-runner/Cargo.toml` | Add feature that forwards to `baml-tool-links/<name>` |
| `crates/baml-rt-builder/Cargo.toml` | Add feature that forwards to `baml-tool-links/<name>` |

That's **5 files**, down from 6 in the original scheme (no source edits to runner/builder/regen_fixtures), and the 3 binary source files never need per-tool edits again. The tool linkage concern is fully isolated in `baml-tool-links`.

**Note:** Runner and builder `Cargo.toml` feature entries become one-liners that forward to `baml-tool-links`:
```toml
# crates/baml-agent-runner/Cargo.toml
[features]
github = ["baml-tool-links/github"]
```
No direct dependency on the tool crate is needed in runner or builder.

---

## 5. CLI Subcommand Details

### 5.1 `new-tool <name>`

Creates a new statically-linked tool crate and patches all necessary files for it to compile and be discoverable.

**Arguments:**
- `<name>` — tool name in kebab-case (e.g. `github`, `jira`, `linear`)
- `--bundle support` — bundle type. Phase 1 supports only `support` (default). Errors with a clear message for other values: "Custom bundles require manual implementation. Only `support` is supported."
- `--with-client` — also scaffold an integration client crate under `crates/integrations/<name>-client/` (Phase 4)
- `--access <level>` — access level: `read` (default), `write`, `delete`
- `--dry-run` — print what would be created/modified without writing

**What it creates:**

```
crates/tools/<name>/
  Cargo.toml
  src/
    lib.rs
```

**What it patches (automated, idempotent, transactional):**

| # | File | Change |
|---|------|--------|
| 1 | `Cargo.toml` (workspace root) | Add `"crates/tools/<name>"` to `members` in sorted position |
| 2 | `crates/baml-tool-links/Cargo.toml` | Add optional dep, feature `<name> = ["dep:baml-tools-<name>"]`, add to `all-tools` |
| 3 | `crates/baml-tool-links/src/lib.rs` | Add `#[cfg(feature = "<name>")] use ::baml_tools_<name> as _;` inside macro |
| 4 | `crates/baml-agent-runner/Cargo.toml` | Add feature `<name> = ["baml-tool-links/<name>"]` |
| 5 | `crates/baml-rt-builder/Cargo.toml` | Add feature `<name> = ["baml-tool-links/<name>"]` |

**Conditional 6th patch:**
| 6 | `crates/integrations/<name>-client/` | Only if `--with-client` is passed (Phase 4) |

**Post-patch validation step:**
After all patches are applied, `new-tool` runs a validation build:
```
cargo check -p baml-tools-<name>
```
If this fails, all patches are rolled back and the error is reported.

**Transactional patching:** All file modifications are collected in memory first. Only after all patches are validated (TOML parses correctly, insertion points found, no duplicates) are files written to disk. If any write fails, previously written files are restored from their in-memory backups.

**Generated `lib.rs` skeleton** (based on clickup/notion patterns):

```rust
//! {Name} tool — `support/{snake_name}`.

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{baml_tool, bundles::Support, tools::BamlTool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// Primary input for the {Name} tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct {Name}Input {
    /// TODO: Define your input fields
    pub query: String,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Output returned by the {Name} tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct {Name}Output {
    pub message: String,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum {Name}Error {
    #[error("{Name} operation failed: {message}")]
    Operation { message: String },
}

impl From<{Name}Error> for BamlRtError {
    fn from(err: {Name}Error) -> Self {
        BamlRtError::ToolExecution(err.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

pub struct {Name}Tool;

impl Default for {Name}Tool {
    fn default() -> Self {
        Self
    }
}

#[baml_tool(
    name = "support/{snake_name}",
    description = "TODO: Describe what this tool does.",
    tags = ["support", "{snake_name}"],
    // Uncomment and fill in if this tool requires API keys:
    // secrets = [
    //     { name = "{UPPER_NAME}_API_KEY", description = "{Name} API token", reason = "Required to authenticate" }
    // ],
    baml_types = [{Name}Input, {Name}Output],
)]
#[async_trait]
impl BamlTool for {Name}Tool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "{snake_name}";
    type OpenInput = ();
    type Input = {Name}Input;
    type Output = {Name}Output;

    fn description(&self) -> &'static str {
        "TODO: Describe what this tool does."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        // TODO: Implement tool logic
        Ok({Name}Output {
            message: format!("Executed with query: {}", args.query),
        })
    }
}
```

### 5.2 `new-agent <name>`

Creates a new agent package under `agents/` with full type generation.

**Arguments:**
- `<name>` — agent name in kebab-case (e.g. `github-agent`, `jira-agent`)
- `--tools <tools>` — comma-separated tool IDs (e.g. `support/github,system/internal_a2a`)
- `--template <template>` — agent pattern template (see section 6)
- `--description <desc>` — human-readable description for discovery

**What it creates:**

```
agents/<name>/
  manifest.json
  tsconfig.json
  baml_src/
    <name>_prompt.baml          # Template-specific BAML functions
    generated_tools.baml        # Auto-generated from tool metadata
  src/
    index.ts                    # Template-specific agent logic
    baml-runtime.d.ts           # Auto-generated TypeScript declarations
```

**Phase 1 implementation:** Wraps the existing `run_bootstrap()` from `bootstrap.rs` directly. Templates `simple` and a basic tools-enabled variant only. The `planner` and `coordinator` templates are Phase 2 deliverables.

**Phase 2 implementation:** Extends with:
1. `planner` template — 3-phase intent/plan/execute pattern from clickup-agent
2. `coordinator` template — multi-agent delegator from coordinator-agent
3. Better generated BAML prompts and `index.ts`

### 5.3 `regen`

Regenerates `generated_tools.baml` and `baml-runtime.d.ts` for all agents.

**No arguments in Phase 1.** Regenerates everything (both `agents/` and `tests/fixtures/agents/`), same as the current `regen_fixtures` binary.

**Future:** An `--agent <name>` flag could enable selective regeneration, but this requires refactoring the `regen_fixtures` logic into a reusable library API with directory filtering. Deferred — full regen is fast enough for the current workspace size.

**Implementation:** Thin wrapper that delegates to the same logic as `regen_fixtures`. Ensures consistency with the nextest setup script in `.config/nextest.toml`.

### 5.4 `list-tools`

Lists all tools registered in the `inventory` catalog with their metadata.

The CLI binary links **all first-party tool crates unconditionally** (via an `all-tools` default feature — see section 7.1). This ensures `list-tools` always shows the complete catalog regardless of which features the runner/builder happen to enable.

```
$ cargo agent-platform list-tools

  support/clickup         Interact with ClickUp workspaces and tasks     [support, clickup]    Write
  support/notion          Read-only Notion access                        [support, notion]     Read
  support/slack           Read-only Slack access                         [support, slack]      Read
  support/calculator      Performs mathematical calculations              [support, calculate]  Read
  system/internal_a2a     Route requests to other agents                 [system]              Write
  system/discover_agents  List available agents                          [system]              Read
  system/discover_tools   List available tools                           [system]              Read
  claude/dev              Claude code session                            [claude]              Write
  memory/recall           Graph-based cognitive memory                   [memory]              Read
  ...
```

### 5.5 `list-agents`

Lists all agent packages under `agents/` and `tests/fixtures/agents/` with their manifest metadata.

```
$ cargo agent-platform list-agents

  clickup-agent           v1.0.0  tools: support/clickup, system/discover_agents, ...
  notion-agent            v1.0.0  tools: support/notion, system/internal_a2a, ...
  coordinator-agent       v1.0.0  tools: system/internal_a2a, system/discover_agents
  claude-session-demo     v1.0.0  tools: claude/dev
  ...
```

### 5.6 `doctor`

Validates workspace integrity with two layers of checks.

**Arguments:**
- `--ci` — exit non-zero on any issue (default in CI)
- `--warn-missing-catalog` — downgrade missing catalog entries from error to warning (escape hatch for unusual builds)

**Layer 1 — Static checks (file-based, no compilation required):**

1. Every tool crate directory in `crates/tools/` is listed in workspace `Cargo.toml` `members`
2. Every tool crate in `crates/tools/` has a matching optional dependency in `crates/baml-tool-links/Cargo.toml`
3. Every tool crate in `crates/tools/` has an entry in `force_link_all_tools!()` macro in `crates/baml-tool-links/src/lib.rs`
4. Every tool feature in `baml-tool-links/Cargo.toml` has a corresponding forwarding feature in `baml-agent-runner/Cargo.toml` and `baml-rt-builder/Cargo.toml`
5. Every feature defined in runner/builder `Cargo.toml` (tool-related) has a corresponding feature in `baml-tool-links`
6. No orphaned features (feature defined but crate missing, or vice versa)

**Layer 2 — Catalog checks (requires compiled inventory):**

7. Every agent's `manifest.json` `tools` array references tool IDs that exist in the compiled inventory

Since the CLI links all first-party tools via `baml-tool-links/all-tools`, a missing catalog entry is a **hard error** in both local and CI modes — it means the tool genuinely doesn't exist or `baml-tool-links` is out of sync. The "feature-disabled build" scenario doesn't apply to the CLI.

```
✗ clickup-agent: tool "support/github" not found in catalog.
  Either the tool doesn't exist, or it's missing from baml-tool-links/all-tools.
  Run 'cargo agent-platform doctor' static checks to diagnose.
```

The `--warn-missing-catalog` flag downgrades this to a warning for edge cases (e.g. running doctor from a non-CLI build context or during development of a new tool before it compiles).

**Layer 3 — Freshness checks (optional, slower):**

8. Generated files (`generated_tools.baml`, `baml-runtime.d.ts`) match what `regen` would produce

This layer runs only when `--ci` is passed (or explicitly requested), since it requires running the type generator.

---

## 6. Agent Templates

### 6.1 `simple` (default when no tools, Phase 1)

Single BAML function, direct response. Uses the existing `bootstrap.rs` `prompt_template_no_tools` and `index_ts_template` as-is.

**Use case:** Chatbot, Q&A agent, summarizer.

### 6.2 `basic-tools` (default when tools are specified, Phase 1)

Uses the existing `bootstrap.rs` `prompt_template_with_tools` and `index_ts_template` with tool support. Wraps `run_bootstrap()` directly.

**Use case:** Simple tool-using agent without multi-step planning.

### 6.3 `planner` (Phase 2)

3-phase architecture modeled on `clickup-agent`:
1. **Intent inference** — BAML function classifies user intent -> `NeedClarification | NotRelevant | {Name}Intent`
2. **Planning** — BAML function produces a step-by-step plan
3. **Execution** — Loop over plan steps using `runGeneratedStepExecutor`

**Generated files include:**
- `baml_src/<name>_prompt.baml` with `Infer{Name}Intent`, `Plan{Name}Work`, `Choose{Name}Action` functions
- `src/index.ts` with the 3-phase `run()` implementation using `__chat_register({ run })`
- Proper `awaitInput` handling for clarification loops

### 6.4 `coordinator` (Phase 2)

Multi-agent delegator pattern modeled on `coordinator-agent`:
- Routes to specialist agents via `system/internal_a2a`
- Uses `system/discover_agents` and `system/discover_tools` for dynamic routing
- Planner decides which specialist to delegate to

---

## 7. Crate Structure

### 7.1 CLI Crate Dependencies and Feature Strategy

The CLI binary links all first-party tool crates to ensure `list-tools` and catalog-based `doctor` checks are always complete. This is acceptable because the CLI is a dev tool, not a production binary.

The CLI depends on `baml-tool-links` with the `all-tools` feature enabled by default. Since `baml-tool-links` is the single source of truth for tool linkage, the CLI's catalog is automatically complete whenever `baml-tool-links/all-tools` is up to date.

```toml
# crates/cargo-agent-platform/Cargo.toml
[dependencies]
baml-tool-links = { path = "../baml-tool-links", features = ["all-tools"] }
baml-rt-tools   = { path = "../baml-rt-tools" }   # for InventoryCatalog
baml-rt-core    = { path = "../baml-rt-core" }     # for AgentManifest
clap            = { version = "4", features = ["derive"] }
toml_edit       = "0.22"
# ...
```

When a new tool is created with `new-tool`, only `baml-tool-links/Cargo.toml` needs the new tool added to `all-tools`. The CLI picks it up automatically because it depends on `baml-tool-links` with `all-tools` enabled.

### 7.2 Module Layout

```
crates/cargo-agent-platform/
  Cargo.toml
  src/
    main.rs               # CLI entry point (clap)
    commands/
      mod.rs
      new_tool.rs          # new-tool subcommand logic
      new_agent.rs         # new-agent subcommand (wraps bootstrap.rs + templates)
      regen.rs             # regen subcommand (wraps regen_fixtures logic)
      list_tools.rs        # list-tools from inventory
      list_agents.rs       # list-agents from filesystem
      doctor.rs            # workspace integrity checker (static + catalog layers)
    templates/
      mod.rs
      tool_lib_rs.rs       # Tool lib.rs template (string interpolation)
      tool_cargo_toml.rs   # Tool Cargo.toml template
      client_lib_rs.rs     # Integration client template (Phase 4)
      agent_simple.rs      # Simple agent template
      agent_planner.rs     # Planner agent template (Phase 2)
      agent_coordinator.rs # Coordinator agent template (Phase 2)
    patchers/
      mod.rs
      workspace_toml.rs    # Patch workspace Cargo.toml members
      tool_links_toml.rs   # Patch baml-tool-links/Cargo.toml deps + features + all-tools
      tool_links_lib.rs    # Patch force_link_all_tools!() macro in baml-tool-links/src/lib.rs
      runner_toml.rs       # Patch baml-agent-runner/Cargo.toml feature forwarding
      builder_toml.rs      # Patch baml-rt-builder/Cargo.toml feature forwarding
    transaction.rs         # Transactional file write with rollback
```

### 7.3 Key Dependencies

- `clap` — CLI parsing with derive
- `toml_edit` — format-preserving in-place TOML patching
- `baml-tool-links` — with `all-tools` feature (ensures complete inventory for `list-tools`, `doctor` catalog layer)
- `baml-rt-tools` — `InventoryCatalog`, `ToolFunctionMetadata` (for `list-tools`, `doctor` catalog layer)
- `baml-rt-core` — `AgentManifest` parsing (for `list-agents`, `doctor`)
- `console` + `indicatif` — terminal output formatting (optional, nice-to-have)

**Not needed:** `syn` — Rust source patching uses regex/string-based insertion (the `force_link_all_tools!()` macro body is a simple pattern).

**Cargo.toml name:** `cargo-agent-platform` — Cargo convention: a binary named `cargo-<name>` is invocable as `cargo <name>`.

---

## 8. Transactional Patching

### 8.1 Problem

`new-tool` patches 5 files (workspace Cargo.toml, baml-tool-links Cargo.toml + lib.rs, runner Cargo.toml, builder Cargo.toml). If patch 3 fails (e.g. insertion point not found), patches 1-2 have already been written, leaving the workspace in an inconsistent state.

### 8.2 Solution

A `TransactionalWriter` that:

1. **Collects** all pending file operations in memory (creates + edits)
2. **Validates** every edit: TOML parses correctly, insertion points found, no duplicate entries
3. **Snapshots** the original content of every file being edited
4. **Writes** all files to disk
5. **On failure:** restores all edited files from snapshots, deletes any created files/directories
6. **On success:** commits (no-op, files are already written)

```rust
struct TransactionalWriter {
    snapshots: Vec<(PathBuf, Option<Vec<u8>>)>,  // (path, original_content_or_None_if_new)
    pending: Vec<PendingWrite>,
}

impl TransactionalWriter {
    fn stage_edit(&mut self, path: PathBuf, content: Vec<u8>) -> Result<()>;
    fn stage_create(&mut self, path: PathBuf, content: Vec<u8>) -> Result<()>;
    fn commit(self) -> Result<()>;
    fn rollback(self);  // Called on Drop if not committed
}
```

---

## 9. File Patching Strategy

### 9.1 TOML Patching (workspace and crate `Cargo.toml`)

Use [`toml_edit`](https://crates.io/crates/toml_edit) which preserves formatting, comments, and ordering.

**Workspace `Cargo.toml`:**
- Parse, find `workspace.members` array
- Insert `"crates/tools/<name>"` in sorted position among existing tool entries
- Write back

**`baml-tool-links/Cargo.toml`:**
- Parse, add to `[dependencies]` section: `baml-tools-<name> = { path = "../tools/<name>", optional = true }`
- Add to `[features]` section: `<name> = ["dep:baml-tools-<name>"]`
- Add `"<name>"` to the `all-tools` feature array
- Optionally add to `http-tools` compound feature if applicable
- Write back

**Runner/Builder `Cargo.toml`:**
- Parse, add to `[features]` section: `<name> = ["baml-tool-links/<name>"]`
- No direct dependency on the tool crate needed (forwarded via `baml-tool-links`)
- Write back

**CLI `Cargo.toml`:**
- No per-tool patching needed. The CLI depends on `baml-tool-links` with `features = ["all-tools"]`.
- New tools are added to `baml-tool-links/all-tools`, which the CLI picks up automatically.

### 9.2 Rust Source Patching (`force_link_all_tools!()` macro)

With the macro centralization, only one Rust source file needs patching: `crates/baml-tool-links/src/lib.rs`.

Strategy: find the `force_link_all_tools!` macro body, locate the feature-gated block, insert the new entry in alphabetical order.

**Pattern to match:**
```rust
        #[cfg(feature = "clickup")]
        use ::baml_tools_clickup as _;
        #[cfg(feature = "memory")]
        use ::baml_tools_memory as _;
        // ... insert new entry here in sorted order
```

Implementation: regex-based — find `#[cfg(feature = "` lines inside the macro, insert at the right alphabetical position.

### 9.3 Idempotency

All patching operations are idempotent:
- If the crate is already a workspace member, skip
- If the dependency already exists, skip
- If the force-link entry already exists, skip
- `doctor` can verify the final state

---

## 10. CI Enhancements

### 10.1 Canonical Feature Set

The CI feature set for tool-dependent operations is defined once in the `justfile` and referenced everywhere:

```just
# Canonical feature set for CI and regen — single source of truth.
# When adding a new feature-gated tool, add it here.
ci-tool-features := "http-tools,memory"

regen:
    cargo run -p baml-rt-builder --features {{ci-tool-features}} --bin regen_fixtures

doctor-ci:
    cargo run -p cargo-agent-platform -- doctor --ci
```

The `rust-ci.yml` workflow references the same set (kept in sync manually, but `doctor --ci` will catch drift — see 10.4).

### 10.2 Generated Code Freshness Check

**New step in `rust-ci.yml`** after nextest:

```yaml
- name: Check generated files are up to date
  run: |
    cargo run -p baml-rt-builder --features http-tools,memory --bin regen_fixtures
    if ! git diff --quiet -- 'agents/**/generated_*.baml' 'agents/**/baml-runtime.d.ts' \
                              'tests/fixtures/agents/**/generated_*.baml' \
                              'tests/fixtures/agents/**/baml-runtime.d.ts'; then
      echo "::error::Generated files are stale. Run 'just regen' and commit the changes."
      git diff --stat -- 'agents/' 'tests/fixtures/agents/'
      exit 1
    fi
```

**Why:** Prevents PRs from landing with stale generated BAML/TypeScript after tool metadata changes.

**Feature coverage:** Uses the same feature set as nextest (`http-tools,memory`). Note that the freshness check runs via `regen_fixtures` (builder binary), not the CLI. The CLI's `doctor` step (10.3) catches tool reference issues independently because the CLI links all tools via `baml-tool-links/all-tools`.

### 10.3 Workspace Integrity Check

**New step** running `doctor` in strict CI mode:

```yaml
- name: Workspace integrity check
  run: cargo run -p cargo-agent-platform -- doctor --ci
```

The `--ci` flag makes `doctor` exit non-zero on any issue. Missing catalog entries are already hard errors by default (the CLI links all tools via `all-tools`), so no additional flag is needed.

### 10.4 What `doctor --ci` Catches

| Check | What it catches |
|-------|----------------|
| Static: workspace members vs crates/tools/ dirs | New tool crate not added to workspace |
| Static: baml-tool-links/Cargo.toml deps | New tool not added to the links crate |
| Static: force_link_all_tools! macro entries | New tool not force-linked (won't appear in inventory) |
| Static: runner/builder feature forwarding | Runner or builder missing feature forwarding to baml-tool-links |
| Catalog: agent manifest tool references | Agent references a tool that doesn't exist or baml-tool-links/all-tools is stale |
| Freshness: generated file diff | Tool types changed but regen not run |

### 10.5 New Tool / New Agent PR Annotations

Add CI annotations that flag PRs touching tool or agent directories:

```yaml
- name: Annotate tool/agent changes
  if: github.event_name == 'pull_request'
  run: |
    TOOLS_CHANGED=$(git diff --name-only origin/main...HEAD | grep -c '^crates/tools/' || true)
    AGENTS_CHANGED=$(git diff --name-only origin/main...HEAD | grep -c '^agents/' || true)
    if [ "$TOOLS_CHANGED" -gt 0 ]; then echo "::notice::PR adds/modifies tool crates"; fi
    if [ "$AGENTS_CHANGED" -gt 0 ]; then echo "::notice::PR adds/modifies agents"; fi
```

### 10.6 Updated CI Pipeline (target state)

```
rust-ci.yml:
  ┌──────────────────────────────────────────────────────┐
  │  Job: nextest                                         │
  │                                                       │
  │  1. Setup (toolchain, deps, fnox.toml)                │
  │  2. cargo +nightly fmt --check                        │
  │  3. cargo clippy --all-targets --all-features         │
  │  4. cargo nextest run --workspace (existing)          │
  │  5. [NEW] regen_fixtures + git diff freshness check   │
  │  6. [NEW] cargo agent-platform doctor --ci   │
  │  7. [NEW] Annotate tool/agent PR changes              │
  │  8. JUnit report                                      │
  └──────────────────────────────────────────────────────┘
```

---

## 11. Integration with Existing Infrastructure

### 11.1 `justfile` Additions

```just
# Canonical feature set for CI and regen — single source of truth.
ci-tool-features := "http-tools,memory"

# SDK CLI shortcuts
new-tool name bundle='support':
    cargo run -p cargo-agent-platform -- new-tool {{name}} --bundle {{bundle}}

new-agent name *args:
    cargo run -p cargo-agent-platform -- new-agent {{name}} {{args}}

regen:
    cargo run -p baml-rt-builder --features {{ci-tool-features}} --bin regen_fixtures

doctor:
    cargo run -p cargo-agent-platform -- doctor

doctor-ci:
    cargo run -p cargo-agent-platform -- doctor --ci

list-tools:
    cargo run -p cargo-agent-platform -- list-tools

list-agents:
    cargo run -p cargo-agent-platform -- list-agents
```

### 11.2 Pre-commit Hook

Add to `.pre-commit-config.yaml`:

```yaml
- id: generated-files-check
  name: Check generated files freshness
  entry: scripts/check-generated-freshness.sh
  language: script
  files: 'crates/tools/.*/src/.*\.rs$|agents/.*/baml_src/.*\.baml$'
  pass_filenames: false
```

This hook runs only when tool source or agent BAML files change, and verifies that `regen` was run.

### 11.3 Nextest Setup Script

The existing `.config/nextest.toml` has a `regen-fixtures` setup script. The `cargo agent-platform regen` command delegates to the same underlying logic, ensuring consistency between CI test runs and manual regen.

---

## 12. Post-Scaffold Validation

### 12.1 `new-tool` Validation

After all patches are applied and committed (transactionally), `new-tool` runs:

```
cargo check -p baml-tools-<name>
```

If this fails, all patches are rolled back and the error is reported with the specific compilation failure.

### 12.2 `new-tool` Regen Validation

After `cargo check` succeeds, `new-tool` runs `regen` for a minimal fixture that references the new tool. It then verifies that the tool's types appear in `generated_tools.baml`.

If the tool metadata is missing from the generated output, this indicates a force-link or inventory issue. The command reports:
```
✗ Tool "support/<name>" metadata not found in generated output.
  This usually means the force-link entry is missing or the crate isn't properly linked.
  Check the force_link_all_tools!() macro in crates/baml-tool-links/src/lib.rs.
```

This validation step catches the case where `baml_gen.rs` or inventory wiring has issues that simple `cargo check` wouldn't catch.

---

## 13. Phased Delivery

### Phase 1: Core CLI + `new-tool` + `baml-tool-links` crate (1.5 weeks)

**Status: PARTIALLY COMPLETE (baml-tool-links foundation done)**

**Completed (commit 095c920):**
1. [x] New `crates/baml-tool-links/` leaf crate with `force_link_all_tools!()` macro + all tool deps
2. [x] Migration of all 3 binary files (runner `main.rs`, builder `baml-agent-builder.rs`, `regen_fixtures.rs`) to use `baml_tool_links::force_link_all_tools!()` instead of manual use lines
3. [x] Runner/builder `Cargo.toml` refactored: direct tool deps replaced with feature forwarding to `baml-tool-links`
4. [x] Added `optional_tool_bundles.rs` modules to encapsulate feature-gated tool initialization
5. [x] Updated test files to import tools via `baml_tool_links::*`
6. [x] Fixed clippy warnings across the codebase

**Remaining:**
7. [ ] `crates/cargo-agent-platform/` crate with `clap` CLI skeleton
8. [ ] `new-tool` subcommand: creates crate + patches 5 files transactionally
9. [ ] `list-tools` subcommand: prints inventory catalog (all tools linked via `all-tools` feature)
10. [ ] `doctor` subcommand: static checks (Layer 1) + catalog checks (Layer 2)
11. [ ] `TransactionalWriter` for safe multi-file patching with rollback
12. [ ] Post-scaffold validation (`cargo check` + regen verification)
13. [ ] Unit tests for template generation, TOML patching, and transactional writer

**Exit criteria:**
- [x] `crates/baml-tool-links/` exists as a workspace member; all 3 binary files use `force_link_all_tools!()`
- [x] Direct tool crate dependencies removed from runner/builder `Cargo.toml` (replaced with feature forwarding)
- [ ] Existing CI passes (migration is behavior-preserving) — needs verification
- [ ] `cargo agent-platform new-tool github` creates a compilable tool crate with all 5 files patched
- [ ] `cargo build -p baml-agent-runner --features github` succeeds
- [ ] `cargo agent-platform list-tools` shows all first-party tools
- [ ] `cargo agent-platform doctor` passes on clean workspace
- [ ] `cargo agent-platform new-tool github --dry-run` prints changes without writing
- [ ] Failed patches roll back cleanly (tested)

### Phase 2: `new-agent` + Planner/Coordinator Templates (1 week)

**Deliverables:**
1. `new-agent` subcommand with `simple` and `basic-tools` templates (wrapping existing `run_bootstrap`)
2. `regen` subcommand wrapping existing regeneration logic
3. `list-agents` subcommand
4. `planner` template based on clickup-agent's 3-phase architecture
5. `coordinator` template based on coordinator-agent's delegator pattern

**Exit criteria:**
- `cargo agent-platform new-agent github-agent --tools support/github --template planner` creates a complete agent package
- The generated agent compiles and can be packaged by `baml-agent-builder`
- `cargo agent-platform regen` updates all generated files
- `cargo agent-platform list-agents` shows all agents with manifests

### Phase 3: CI Integration (3-5 days)

**Deliverables:**
1. Generated code freshness check in `rust-ci.yml`
2. `doctor --ci` step in CI pipeline
3. Pre-commit hook for generated file freshness
4. `justfile` shortcuts with canonical `ci-tool-features` variable
5. PR annotation step for tool/agent changes
6. Updated `CLAUDE.md` with SDK CLI documentation

**Exit criteria:**
- CI fails if generated files are stale
- CI fails if workspace integrity is broken (orphaned features, missing force-link entries)
- `just new-tool`, `just new-agent`, `just regen`, `just doctor` all work
- `just doctor-ci` matches what CI runs

### Phase 4: Polish + Advanced Features (1 week, optional)

**Deliverables:**
1. Interactive mode for `new-tool` and `new-agent` (using `inquire` crate — already a dependency in the builder)
2. `--with-client` flag for `new-tool` (scaffolds integration client crate under `crates/integrations/`)
3. `new-tool --with-tests` flag (scaffolds integration test file)
4. Better error messages and recovery suggestions
5. `cargo agent-platform update-tool <name>` — re-patches linkage if manual changes drifted
6. `--check` mode for `new-tool` / `new-agent` (validate-only, no writes — useful for CI testing of the scaffolding logic itself)

**Exit criteria:**
- Interactive mode guides user through tool/agent creation with prompts
- Generated integration client follows the `clickup-client`/`notion-read` pattern
- `--check` mode exits 0 if the operation would succeed, non-zero otherwise

---

## 14. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| TOML patching breaks formatting | Medium | Low | Use `toml_edit` (format-preserving); test against actual workspace files |
| `force_link_all_tools!()` macro body patching inserts in wrong location | Low | Medium | Regex-based insertion with alphabetical ordering; `doctor` validates; rollback on failure |
| Template goes stale as platform evolves | High | Medium | Phase 1 uses existing `bootstrap.rs` templates; `doctor` validates compilation |
| CLI binary links all tools — slower compile | Low | Low | CLI is a dev tool, not in `default-members`; compile once, use many times |
| `baml-tool-links` migration breaks existing builds | Low | High | Migration is a mechanical refactor (behavior-preserving); CI validates immediately |
| Post-scaffold `cargo check` is slow | Medium | Low | Only runs once after scaffold; user expects compilation on new crate creation |
| Transactional rollback fails to restore on disk error | Very Low | Medium | Best-effort restoration; original content in memory; user can `git checkout` |
| Feature forwarding adds indirection | Low | Low | One extra hop in Cargo feature resolution; no runtime cost; clearer ownership |

---

## 15. Non-Goals (Deferred)

1. **WASM external tools** — deferred to a later phase. The CLI architecture supports adding `new-external-tool` later.
2. **Publishing tools to a registry** — no tool distribution mechanism in this phase.
3. **Hot-reloading tools** — tools are statically linked; recompilation is required.
4. **Agent marketplace / template registry** — templates are built-in for now.
5. **GUI / web-based scaffolding** — CLI only.
6. **`--bundle` values other than `support`** — almost all third-party tools use `support`. Custom bundles require manual implementation.
7. **`--agent` flag on `regen`** — requires refactoring `regen_fixtures` into a library with directory filtering. Current full-regen is fast enough.

---

## 16. Success Metrics

1. **Time to first tool**: from zero to a compilable, registered tool in under 2 minutes (currently ~30 minutes manual work)
2. **Time to first agent**: from zero to a runnable agent package in under 2 minutes (currently ~15 minutes with bootstrap, longer without)
3. **CI catches stale generated code**: 100% of PRs with tool metadata changes that forgot `regen` are caught before merge
4. **Zero manual file edits for tool linkage**: `new-tool` handles all 5 file patches + post-scaffold validation automatically
5. **Force-link consistency**: `baml-tool-links` crate with `force_link_all_tools!()` eliminates the class of bugs where a tool compiles but isn't discoverable because someone forgot one of the 3 force-link sites
