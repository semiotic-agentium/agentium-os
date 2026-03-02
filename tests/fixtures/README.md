# Test Fixtures

This directory contains test fixtures used by the test suite.

## Structure

- **`agents/`** — Complete test agent applications (each has `baml_src/`, `src/`, `manifest.json`, `tsconfig.json`):
  - **`task-lifecycle-demo`** — **Reference for conversation handling.** Full task lifecycle: `__chat_register({ run })`, `ctx.emit.awaitInput()`, sequential review and sign-off loops, artifacts. Best example to copy for multi-turn A2A flows. See `src/index.ts`.
  - `stream-baml-tool` — BAML tool (FSM) + streaming response; also used for QuickJS BAML invoke/stream tests (ChooseCalcTool, ChooseCalcToolStream)
  - `stream-js-tool` — JS-only streaming (emit, artifact, no BAML tools)
  - `slack-smoke-tool` — Minimal fixture that declares `support/slack`; used to verify Slack tool metadata/typegen/package wiring in fixture build flows.
  - `conversational-context-auto` — Multi-turn chat with BAML functions and tool routing
  - `conversational-persona-demo` — Persona-based chat
  - **`tool-discovery-demo`** — Uses `system/discover_tools` + BAML prompt to find tools by query (e.g. "Notion", "calculate"). Ad hoc verification: `./scripts/verify-tool-discovery.sh --build`.
  - `emit-plan-then-block` — Fixture that emits plan chunks then blocks the event loop (no yield) before returning; used in A2A stream tests for relay flush timing.

  Each agent’s `src/baml-runtime.d.ts` is **generated** from its `baml_src/` by the runtime type generator. To refresh all fixture declarations after changing the generator or BAML:

  ```bash
  cargo run -p baml-rt-builder --bin regen_fixtures
  ```

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
2. Add `{name}` to the `regen_fixtures` binary in `crates/baml-rt-builder/src/bin/regen_fixtures.rs`.
3. Run `cargo run -p baml-rt-builder --bin regen_fixtures` to generate `src/baml-runtime.d.ts`.
4. Update this README if adding new categories.
