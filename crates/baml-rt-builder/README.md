# baml-rt-builder

Agent build pipeline and packaging utilities.

## Responsibilities

- TypeScript/JavaScript linting and compilation via OXC.
- BAML type generation and schema handling for packaging.
- **Runtime type generation**: typed `baml-runtime.d.ts` from BAML runtime IR (no BAML source parsing).
- Agent packaging into distributable archives.

## Generated BAML Interfaces

The builder emits `generated_tools.baml`, which includes `ToolSessionPlan`
and `ToolSessionStep` definitions used by BAML to describe host tool session
execution steps.

## Generated TypeScript Declarations (`baml-runtime.d.ts`)

The runtime type generator produces `dist/baml-runtime.d.ts` with:

- **Typed BAML function declarations** (args and return types from IR), wrapped in `declare global { }` so they are visible when the file is used as a module.
- **Supporting types**: interfaces, enums, and type aliases for BAML classes/unions.
- **Host tool session API**: `openToolSession`, tool-specific openers, and shared types (e.g. `ToolFailure`, `ToolStep`, `ToolSession`).
- **Comments**: file header and section comments (BAML types, BAML functions, host tool API).

Agent code (e.g. `index.ts`) can call `await MyBamlFunction(args)` with full TypeScript types.

## Binaries

- **`baml-agent-builder`**: CLI entry point — lint, compile, and package agents.
- **`regen_fixtures`**: Regenerates `baml-runtime.d.ts` for all fixture agents under `tests/fixtures/agents/` (stream-baml-tool, stream-js-tool, conversational-context-auto, conversational-persona-demo). Run after changing the generator or BAML fixtures to keep checked-in declarations up to date:

  ```bash
  cargo run -p baml-rt-builder --bin regen_fixtures
  ```
