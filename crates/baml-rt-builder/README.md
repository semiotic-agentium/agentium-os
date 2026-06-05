# baml-rt-builder

Agent build pipeline and packaging utilities.

**Authoring agents:** [`docs/assertions/how-to-write-agents.md`](../../docs/assertions/how-to-write-agents.md) — when to run `regen_fixtures`, `steps` vs `plan_steps`, and generated artifacts.

## Responsibilities

- TypeScript/JavaScript linting and compilation via OXC.
- BAML type generation and schema handling for packaging.
- **Runtime type generation**: typed `baml-runtime.d.ts` from BAML runtime IR (no BAML source parsing).
- Agent packaging into distributable archives.

## Generated BAML prelude (`_baml_runtime.baml`)

Single file, analogous to `baml-runtime.d.ts`: shared types, tool interfaces, optional session coordination,
polymorphic session unions, and per-phase executors. Section markers: `// ── builder: …`.

`regen_fixtures` syncs it into each agent `baml_src/` and removes legacy split files (`generated_tools.baml`, …).

## Generated TypeScript Declarations (`baml-runtime.d.ts`)

The runtime type generator produces `dist/baml-runtime.d.ts` with:

- **Typed BAML function declarations** (args and return types from IR), wrapped in `declare global { }` so they are visible when the file is used as a module.
- **Supporting types**: interfaces, enums, and type aliases for BAML classes/unions.
- **A2A task DSL**: `session(message)`, `RunContext`, `SessionEmitter`, `SessionResult`; `__chat_register({ run })` so the agent entrypoint is `run(ctx)` with `ctx.text`, `ctx.message`, `ctx.emit`; `messageText(message)` and `message.text()` for first-text extraction; `emit.awaitInput(prompt)` for INPUT_REQUIRED suspension. All in one file; no separate `a2a.ts`.
- **Host tool session API**: `openToolSession`, tool-specific openers, and shared types (e.g. `ToolFailure`, `ToolStep`, `ToolSession`).

Agent code uses `__chat_register({ run: async (ctx) => { ... } })` or `session(message).run(...)` and calls `await MyBamlFunction(args)` with full types. See the how-to doc for DSL overview; **task-lifecycle-demo** (`tests/fixtures/agents/task-lifecycle-demo/src/index.ts`) remains the detailed lifecycle reference.

## Binaries

- **`baml-agent-builder`**: CLI entry point — lint, compile, and package agents.
- **`regen_fixtures`**: Regenerates `baml-runtime.d.ts` and `_baml_runtime.baml` for every directory under `tests/fixtures/agents/` and `agents/` that has `baml_src/`.
  - Default mode (no args): scan the two roots above.
  - Targeted mode: pass one or more explicit agent directories with `--path <agent-dir>`.
  Run after changing the generator, prelude, or agent BAML. **Pass `--all-features` (or at least `http-tools`)** so optional tool crates link and manifest tools resolve (e.g. `support/crm` / `support/slack`).
  If an agent references external tools, also set `BAML_EXTERNAL_TOOLS_DIR` to the external tool directory (or a colon-separated list of tool directories containing `tool-manifest.json`).

  ```bash
  cargo run -p baml-rt-builder --all-features --bin regen_fixtures
  cargo run -p baml-rt-builder --all-features --bin regen_fixtures --path examples/agents/echo-agent
  ```

  Workspace shortcut: `just regen-fixtures`.
