# Test Fixtures

This directory contains test fixtures used by the test suite.

## Structure

- **`agents/`** — Complete test agent applications (each has `baml_src/`, `src/`, `manifest.json`, `tsconfig.json`):
  - **`task-lifecycle-demo`** — **Reference for conversation handling.** Full task lifecycle: `__chat_register({ run })`, `ctx.emit.awaitInput()`, sequential review and sign-off loops, artifacts. Best example to copy for multi-turn A2A flows. See `src/index.ts`.
  - `stream-baml-tool` — BAML tool (FSM) + streaming response; also used for QuickJS BAML invoke/stream tests (ChooseCalcTool, ChooseCalcToolStream)
  - `stream-js-tool` — JS-only streaming (emit, artifact, no BAML tools)
  - `slack-smoke-tool` — Minimal fixture that declares `support/slack`; used to verify Slack tool metadata/typegen/package wiring in fixture build flows.
  - `conversational-context-auto` — Multi-turn chat with BAML functions and tool routing
  - `security-eval-agent` — Declares `support/crm` + `support/email` (security-eval tools); included in `just coordinator-claude-extrospection*` / `just coordinator-claude-extrospection-clickup` (aliases: `just persona-claude-*`) alongside the main dev agents.
  - **`tool-discovery-demo`** — Uses `system/discover_tools` + BAML prompt to find tools by query (e.g. "Notion", "calculate"). Ad hoc verification: `./scripts/verify-tool-discovery.sh --build`.
  - `emit-plan-then-block` — Fixture that emits plan chunks then blocks the event loop (no yield) before returning; used in A2A stream tests for relay flush timing.

  Each agent’s `src/baml-runtime.d.ts` and **`baml_src/_baml_runtime.baml`** are **generated** from manifest + hand-written BAML. To refresh all fixture trees after changing the generator, prelude, or agent BAML:

  ```bash
  just regen-fixtures
  # or: cargo run -p baml-rt-builder --all-features --bin regen_fixtures
  ```

  `regen_fixtures` must link every tool crate your fixtures declare (e.g. `support/crm` / `support/email` need `security-eval`; `support/slack` needs `slack`). **`--all-features`** (or at least `http-tools`) avoids “Tool metadata missing” failures.

  **Pre-commit:** With `pre-commit install`, the `regen-fixtures` hook runs `regen_fixtures` when staged files match builder/tools/agent paths (see `.pre-commit-config.yaml`) and fails if `agents/` or `tests/fixtures/agents/` would change—stage the regenerated files and commit again. Skip (emergency only): `SKIP=regen-fixtures git commit …`. CI does **not** run regen; keep generated outputs committed.

- **`baml/`** — BAML schema fixtures (if present)
  - Used for schema-level tests

- **`packages/`** — Pre-built test packages (generated during tests)
  - For packages created during test execution

## Usage

Use the fixture helpers in `test-support` and test modules:

```rust
let baml_src = fixture_baml_src("stream-baml-tool");  // path to agents/stream-baml-tool/baml_src
let agent_root = agent_fixture("stream-baml-tool");   // path to agents/stream-baml-tool
```

## Adding New Fixtures

1. Add an agent under `agents/{name}/` with `baml_src/`, `src/`, `manifest.json`, `tsconfig.json`.
2. If the agent declares tools that need optional crates, ensure `regen_fixtures` / `baml-agent-builder` link those crates (`crates/baml-rt-builder/src/bin/regen_fixtures.rs` uses `#[cfg(feature = …)] use baml_tools_* as _;`).
3. Run `just regen-fixtures` (or `cargo run -p baml-rt-builder --all-features --bin regen_fixtures`) to emit `_baml_runtime.baml` and `src/baml-runtime.d.ts`.
4. Update this README if adding new categories.
