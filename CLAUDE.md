# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Build (requires Rust nightly — edition 2024; use the flake.nix devShell)
cargo build
cargo build --release

# Test (source .env first for API-key-dependent tests)
set -a && source .env && set +a
cargo test                                     # runs default-members only
cargo test --workspace                         # runs all crates
cargo test -- --nocapture

# Run a single test
cargo test test_name
cargo test -p baml-rt test_name               # specific crate
cargo test -p baml-rt-a2a test_name -- --nocapture

# Feature-gated test suites
cargo test -p baml-rt-provenance --features falkordb-tests -j 1

# Lint (run before committing)
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check

# Full pre-commit checks (fmt, clippy, typos, cargo-check)
pre-commit run --all-files

# Snapshot testing (provenance crate uses insta)
cargo insta review

# Regenerate TypeScript type declarations from baml_src/ changes
cargo run -p baml-rt-builder --bin regen_fixtures

# Binaries
cargo run -p baml-rt-builder --bin baml-agent-builder   # lint, compile, package agents
cargo run -p baml-agent-runner                           # load packaged agents, serve A2A

# FalkorDB for provenance graph tests
./scripts/falkordb.sh              # commands: up, down, restart, status, logs
```

## Architecture

This is a Rust workspace (edition 2024, requires nightly) for the BAML agent runtime — executing BAML functions, running JavaScript agents via QuickJS, tool orchestration, and serving A2A (agent-to-agent) protocol requests.

### Crate Dependency Graph (bottom-up)

- **baml-rt-core** — Shared error types, result types, correlation helpers
- **baml-rt-id** — Newtype ID wrappers (UUID-based)
- **baml-rt-tools** — Tool trait, registry/executor, session FSM (`ToolSessionPlan` with Open/Send/Next/Finish/Abort ops)
- **baml-rt-interceptor** — Interceptor trait + pipeline (pre/post execution hooks)
- **baml-rt-observability** — OpenTelemetry tracing setup, spans, metrics
- **baml-rt-quickjs** — QuickJS runtime host: loads JS, bridges JS↔Rust, manages BAML runtime invocations
- **baml-rt-a2a** — Agent-to-agent protocol: JSON-RPC types, SSE streaming transport, streaming task handling
- **baml-rt-provenance** — Provenance graph: event normalization, FalkorDB persistence via text-to-cypher
- **baml-rt-api** — HTTP API surface: agent discovery (GET /agents), A2A JSON-RPC forwarding, OpenAPI via utoipa, RFC 7807 errors
- **baml-rt-builder** — Agent build pipeline: OXC lint/compile TypeScript, BAML type generation, tar.gz packaging. Binary: `baml-agent-builder`
- **baml-agent-runner** — Loads packaged agent tar.gz, serves A2A requests. Binary: `baml-agent-runner`
- **baml-rt** — Facade crate re-exporting subcrates via feature flags (default: all enabled)
- **baml-derive-core** — Core types and rendering for derive macro (`BamlType` trait)
- **baml-derive** — Proc-macro: `#[derive(BamlType)]` with `#[baml(dynamic)]`, `#[baml(union)]`, `#[baml(alias)]`, `#[baml(skip)]` etc.
- **baml-derive-tests** — Integration tests for derive macro (not published)
- **tools/internal-dev** — Test tool implementations (Calculator, Delay, Uppercase, Weather, A2aRelay) registered via `inventory`
- **test-support** — Shared test fixtures and helpers (not published)

### Key Runtime Flow

JS code → `QuickJSBridge` → checks `globalThis` for JS function → if missing, falls back to `BamlRuntimeManager` → runs interceptor pipeline → calls BAML runtime → LLM provider → tool session execution (host tools run in Rust, never JS) → interceptor post-hooks → result back to JS as resolved Promise.

### Host Tool Contract

Host tools are session-based. BAML returns a declarative `ToolSessionPlan` describing FSM steps (Open → Send* → Next → Finish/Abort). The Rust runtime executes these steps; JavaScript never mediates host tool execution.

### Conversation Handling (A2A DSL) — Reference Example

The **best example** of multi-turn conversation and task lifecycle is the **task-lifecycle-demo** fixture: `tests/fixtures/agents/task-lifecycle-demo/src/index.ts`.

- **Entrypoint:** `__chat_register({ run })` — the agent implements `run(ctx: RunContext)`; the runtime wraps it into `onChatMessage`. No boilerplate `session(message).run(...)` in agent code.
- **Context:** `ctx.text` (first text part), `ctx.message` (inbound message), `ctx.emit` (message, artifact, `awaitInput`).
- **Suspension:** `await ctx.emit.awaitInput(prompt)` emits INPUT_REQUIRED and resumes when the next message is routed to the same task/context.
- **Helpers:** `messageText(message)` for any message; `session(message).text()` for the initial message. Messages from `awaitInput` have `.text()`.
- **Flow:** Path choice → review loop → sign-off loop → COMPLETED (sequential loops, no nesting).

Other fixtures (stream-js-tool, stream-baml-tool, conversational-context-auto, etc.) use the same DSL; task-lifecycle-demo is the most complete reference.

### Feature Flags (baml-rt facade)

- `tools` → baml-rt-tools
- `interceptor` → baml-rt-interceptor
- `observability` → baml-rt-observability
- `quickjs` → baml-rt-quickjs (implies tools + interceptor + observability)
- `a2a` → baml-rt-a2a (implies quickjs)
- `builder` → baml-rt-builder (implies observability)

### Test-Gating Feature Flags

- `falkordb-tests` — FalkorDB-dependent tests using testcontainers; run in dedicated CI job
- `http-tools` — HTTP-dependent tools (ClickUp, Notion); tested separately in CI

## CI Structure

Three parallel jobs in `rust-ci.yml` (push/PR to main):

1. **cargo-test (light)** — `baml-rt-core`, `baml-rt-id`, `baml-rt-observability`, `baml-derive-tests`
2. **cargo-test-heavy (serial, -j 1)** — Remaining workspace crates, http-tools tests, baml-rt-a2a
3. **cargo-test-falkordb (serial, -j 1)** — `--features falkordb-tests` for provenance, a2a, runner

All jobs use sccache (GHA backend) + rust-cache with shared key.

## Testing Conventions

- **Vertical slices over unit shards**: test via public API surfaces (`BamlRuntimeManager`, `QuickJSBridge`, `A2aRequestHandler`), not internal shortcuts
- **test-support crate**: use `setup_baml_runtime_default()`, `setup_baml_runtime_from_fixture()`, `setup_bridge()`, `agent_fixture()`, `require_api_key()`, `ensure_fixture_runtime_types()`, `ensure_baml_src_exists()`
- **Call `ensure_fixture_runtime_types()`** at the start of any E2E test loading from `tests/fixtures/agents/`
- **Async tests**: use `#[tokio::test]`
- **Snapshot tests**: `insta::assert_json_snapshot!` in provenance crate; update with `cargo insta review`
- **Property tests**: scope attribution, tool session lifecycle, stream ordering (using proptest)
- **Test fixtures**: `tests/fixtures/agents/` (agent packages), `baml_src/` (BAML schemas)
- **FalkorDB tests**: use testcontainers, start via `scripts/falkordb.sh` or automatically

## Rust Conventions

- Use `dotenvy` (not dotenv)
- Use named string interpolation: `format!("{name} is {value}")` not `format!("{} is {}", name, value)`
- Never unwrap in production code; use `?` with proper error types
- Never silently discard errors with `let _ =` without logging
- Error variant names should describe the operation that failed (e.g., `VaultRetrieval`, not `External`)
- Use discriminated unions / enums over Option fields to make invalid states unrepresentable
- Use newtype wrappers for domain IDs and values
- Structured logging: static messages with dynamic data in fields (`tracing::info!(from = %from, event = "payment")`)
- No version history in comments; describe current behavior in present tense
- `#[allow(dead_code)]` requires a justifying comment explaining why the code is reserved

## External Dependencies

- **BAML runtime**: pinned git rev from ryan-s-roberts/baml (canary branch)
- **OXC**: TypeScript parsing/compilation (oxc_parser, oxc_codegen, oxc_transformer, oxc_semantic)
- **QuickJS**: `quickjs_runtime` crate for JS execution
- **text-to-cypher**: Cypher query generation for FalkorDB provenance graphs
- **testcontainers**: FalkorDB integration tests
