# BAML Runtime

BAML Runtime is a Rust workspace that hosts BAML execution, QuickJS agent
integration, tool systems, and A2A protocol handling. The public entry point is
the `baml-rt` facade crate, which re-exports feature-gated subcrates.

## Workspace Architecture

### Crate Map (Bottom-Up)

- `baml-rt-core`: Core errors/results, correlation helpers, stream boundary types, and effect-bus primitives.
- `baml-rt-tools`: Tool traits, registry/executor, and session FSM primitives.
- `baml-rt-interceptor`: Interceptor traits, pipelines, and tracing interceptors.
- `baml-rt-observability`: Tracing setup, spans, and metrics helpers.
- `baml-rt-quickjs`: QuickJS runtime host, schema loading, JS bridge, and async stream execution.
- `baml-rt-a2a`: Agent-to-agent protocol types, stream-first transport, and cross-turn request handling.
- `baml-rt-builder`: Agent build pipeline and `baml-agent-builder` CLI.
- `baml-agent-runner`: Binary that loads packaged agents and serves A2A requests.
- `baml-rt`: Facade crate that re-exports the above via feature flags.
- `test-support`: Shared fixtures and helper utilities for tests.

### Runtime Flow (QuickJS + BAML)

```mermaid
sequenceDiagram
    participant JS as JavaScript Code
    participant QJS as QuickJSBridge
    participant BRM as BamlRuntimeManager
    participant BR as BamlRuntime
    participant TR as ToolRegistry
    participant INT as Interceptors
    participant LLM as LLM Provider

    JS->>QJS: invoke_function("greetUser", args)
    QJS->>QJS: check globalThis["greetUser"]
    alt JS function exists
        QJS->>JS: call function
        JS-->>QJS: Promise result
    else fallback to BAML
        QJS->>BRM: invoke BAML function
        BRM->>INT: pre-execution interceptors
        BRM->>BR: call BAML runtime
        BR-->>LLM: provider request
        LLM-->>BR: response
        BR-->>BRM: result
    BRM->>TR: tool session execution (if any)
        BRM->>INT: post-execution interceptors
        BRM-->>QJS: result
    end
    QJS-->>JS: Promise resolves
```

### Build + Packaging Flow

```mermaid
graph LR
    subgraph "Agent Source"
        SRC[baml_src/*.baml]
        TS_SRC[src/*.ts]
        MAN[manifest.json]
    end

    subgraph "baml-rt-builder"
        LINT[OXC lint]
        TYPE_GEN[BAML type gen]
        TS_COMP[OXC compile]
        PACK[Packager]
    end

    subgraph "Output"
        BAML_IL[baml_src IL]
        TYPES[dist/baml-runtime.d.ts]
        JS[dist/*.js]
        PKG[agent.tar.gz]
    end

    SRC --> TYPE_GEN
    SRC --> BAML_IL
    TS_SRC --> LINT --> TS_COMP --> JS
    TYPE_GEN --> TYPES
    MAN --> PACK
    BAML_IL --> PACK
    TYPES --> PACK
    JS --> PACK
    PACK --> PKG
```

## Facade Features (`baml-rt`)

The `baml-rt` crate exposes feature-gated modules:

- `tools` → `baml-rt-tools`
- `interceptor` → `baml-rt-interceptor`
- `quickjs` → `baml-rt-quickjs` (implies tools/interceptor/observability)
- `a2a` → `baml-rt-a2a` (depends on `quickjs`)
- `builder` → `baml-rt-builder`
- `observability` → `baml-rt-observability`

Default features enable all of the above.

## BAML ↔ Host Tool Contract

Host tools are **session-based**. BAML returns a declarative `ToolSessionPlan`
that describes FSM steps (`Open`, `Send`, `Next`, `Finish`, `Abort`), and the
runtime executes those steps **in Rust**. JavaScript never mediates host tool
execution; JS only handles JS tools via `invokeTool`.

`Open.initial_input` is open-time session configuration (for tools that define
it). `Send.input` carries per-turn payloads.

## Promise polling and effect-gated timeout

When `evaluate()` runs non-stream code that returns a promise, the host polls until
the promise resolves or a timeout. The timeout is **effect-gated** and **deterministic**:
the first timeout sample is taken only after one run of pending jobs (so the promise
executor has had a chance to emit effects); then the effect state is re-checked on a
fixed schedule (every 10 attempts for the first 500ms, then every 100). If
in-flight effects (e.g. LLM or tool calls) are present for the invocation context,
a long timeout is used; otherwise a short idle timeout applies. See
`crates/baml-rt-quickjs/docs/HOST_QUICKJS_STREAM_INVARIANTS.md` and
`crates/baml-rt-quickjs/src/quickjs_bridge/promise_polling.rs`.

**Potential problem:** The Rust future that backs the JS promise (e.g. the BAML/LLM
call) only runs when the QuickJS event loop runs pending jobs. On slow or busy CI,
that can happen well after the first timeout sample, so the poll loop may see no
effects and use the short idle timeout (e.g. 5s), causing “Promise did not resolve
after 5000 attempts”. To mitigate this, the first 2 seconds of polling never use
the short timeout (warm-up window). To isolate locally, run with
`RUST_LOG=baml_rt_quickjs=trace` and check for “LlmStarted emitting” vs
“poll_promise: effect-gated timeout sample” timing (see `crates/baml-rt/tests/llm_test.rs`).

## Conversation Handling (A2A DSL)

The **best reference** for multi-turn conversation and task lifecycle is the
**task-lifecycle-demo** fixture:

- **Path:** `tests/fixtures/agents/task-lifecycle-demo/src/index.ts`
- **Concepts:** `__chat_register({ run })` with a single `run(ctx)` entrypoint;
  `ctx.text`, `ctx.message`, `ctx.emit` for working messages, artifacts, and
  `await emit.awaitInput(prompt)` to suspend until the next user message.
- **Flow:** Path choice → review loop (approve/reject/revise) → sign-off loop
  (confirm/request-changes/cancel) → COMPLETED. No nested loops; sequential
  phases only.

All fixture agents under `tests/fixtures/agents/` use the same DSL (see
`tests/fixtures/README.md`). Types and runtime are in the generated
`baml-runtime.d.ts`; there is no separate `a2a.ts`.

## Binaries

- `baml-agent-builder` (from `baml-rt-builder`): Lint, compile, and package agents.
- `baml-agent-runner` (from `baml-agent-runner`): Load packaged agents and serve A2A.

## Repository Layout

```text
baml-rt/
├── crates/
├── baml_src/                    # Example BAML schemas
├── tests/fixtures/agents/       # Example/demo agents (task-lifecycle-demo, stream-*, etc.)
└── tests/                       # Workspace-level tests and fixtures
```

## Development Commands (justfile)

The project uses [just](https://github.com/casey/just) as a command runner. A `.env` file is loaded automatically (`set dotenv-load`). Run `just --list` to see all available recipes.

| Recipe | Usage | Description |
|---|---|---|
| `just fmt` | `just fmt` | Run `cargo fmt --all` to format the entire workspace. |
| `just test` | `just test` | Full nextest suite with CI-parity feature flags. Requires `cargo-nextest`, a running FalkorDB on `localhost:6379`, and `OPENROUTER_API_KEY` for LLM tests. |
| `just test-build` | `just test-build` | Compile-only (no execution) — useful as a quick pre-push sanity check. |
| `just test-crate <crate>` | `just test-crate baml-rt-provenance` | Run tests for a single crate with the same CI feature flags. |
| `just test-unit` | `just test-unit` | Run only unit tests that need neither FalkorDB nor API keys. |
| `just clickup-agent` | `just clickup-agent` | Build and run the ClickUp agent: packages it via `baml-agent-builder` then launches the runner in A2A stdio mode. Uses in-memory SQLite provenance by default. |
| `just clickup-agent-provenance` | `just clickup-agent-provenance` | Same as `clickup-agent`, but persists provenance to `provenance.db`. |
| `just notion-agent` | `just notion-agent` | Build and run the Notion agent in A2A stdio mode (HTTP tools enabled). Uses in-memory provenance by default. |
| `just notion-agent-provenance` | `just notion-agent-provenance` | Same as `notion-agent`, but persists provenance to `provenance.db`. |
| `just notion-demo` | `just notion-demo` | Start the Notion HTTP demo runner and stream one request; writes SSE output and captures context/task IDs when present. |
| `just notion-demo-stop` | `just notion-demo-stop` | Stop the background runner started by `notion-demo`. |
| `just provenance-mermaid <context_id>` | `just provenance-mermaid ctx-1771426017780-2` | Export a simplified Mermaid sequence diagram for a given provenance context ID. |

For a provenance-first walkthrough of the Notion flow, see `docs/notion-demo.md`.
For the next-stage Notion demo/UX strategy and invariants, see `docs/notion-experience-blueprint.md`.

### CI feature flags

The `test`, `test-build`, and `test-crate` recipes enable the following features to match CI:

```text
baml-rt-tools/http-tools, baml-rt-builder/http-tools, baml-rt-provenance/falkordb-tests,
baml-rt-a2a/falkordb-tests, baml-agent-runner/falkordb-tests, baml-agent-runner/http-tools,
baml-rt/llm-tests, baml-agent-runner/llm-tests
```

## Testing

Source `.env` before running tests so API-key–dependent e2e and contract tests pass (e.g. `OPENROUTER_API_KEY`):

```bash
set -a && source .env && set +a
cargo test
# Note: this runs only workspace `default-members` now. To run everything:
# cargo test --workspace

# With output
cargo test -- --nocapture
```

**Slow tests / pre-effect timeout:** Contract and LLM-dependent tests (e.g. `contracts_test::test_js_function_invocation_returns_actual_result`) can be slow or fail because they hit the **short (idle) timeout before any LLM effect is observed**: the promise executor may not run soon enough, so the poll loop never sees `LlmStarted` and uses the idle timeout (e.g. 5s default or 45s if configured). That can cause 60s+ wall time (e.g. multiple timeouts or one long idle limit) or failure with "Promise did not resolve after N attempts". The warm-up window (first 2s never use the short timeout) and effect wiring (see "Promise polling and effect-gated timeout" above) mitigate this; ensure tests that call BAML/LLM use a QuickJS config with long enough timeouts and wire `set_effect_liveness`. Running with `cargo test --release` reduces CPU-bound time but does not fix the pre-effect timeout issue. CI uses nextest with the `llm` test group limited to 2 threads.

## License

[License information]
