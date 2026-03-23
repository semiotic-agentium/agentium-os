# baml-rt-builder

Agent build pipeline and packaging utilities.

## Responsibilities

- TypeScript/JavaScript linting and compilation via OXC.
- BAML type generation and schema handling for packaging.
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

Agent code uses `__chat_register({ run: async (ctx) => { ... } })` or `session(message).run(...)` and calls `await MyBamlFunction(args)` with full types. The **task-lifecycle-demo** fixture (`tests/fixtures/agents/task-lifecycle-demo/src/index.ts`) is the reference for conversation handling.

## Binaries

- **`baml-agent-builder`**: CLI entry point — lint, compile, and package agents.
- **`regen_fixtures`**: Regenerates `baml-runtime.d.ts` for all fixture agents under `tests/fixtures/agents/` (task-lifecycle-demo, stream-baml-tool, stream-js-tool, conversational-context-auto, conversational-persona-demo). Run after changing the generator or BAML fixtures to keep checked-in declarations up to date:

  ```bash
  cargo run -p baml-rt-builder --bin regen_fixtures
  ```

  Also refreshes checked-in `_baml_runtime.baml` where applicable.
