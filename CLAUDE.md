Looking at this merge, I can see several significant architectural changes:

1. **New crates**: `baml-rt-core` with new types like `step_executor_outcome.rs` and `ids.rs`
2. **Major provenance refactoring**: New `metamodel/` module with comprehensive query system
3. **Task update system**: New files like `task_update_broadcaster.rs`, `task_update_drain.rs`, `task_update_session.rs`
4. **Web dashboard**: New narrative dashboard with provenance drilldown capabilities
5. **Removed fixture**: `conversational-persona-demo` agent fixture removed
6. **New unified harness**: `unified-step-harness-demo` fixture added
7. **BAML conversation history changes**: Script now rejects `ctx.tags['conversation_history']` entirely
8. **TypeScript config updates**: All `tsconfig.json` files updated (likely moduleResolution changes)

These changes represent significant architectural evolution in the provenance system, task management, and web interface. The CLAUDE.md updates in the merge show the TypeScript 6.x requirement description changed from `ignoreDeprecations: "6.0"` to `moduleResolution: "bundler"`, and test model updated from `grok-4.1-fast` to `grok-4.3`.

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Build (nightly pinned via rust-toolchain.toml)
cargo build
cargo build --release

# Test — secrets resolved via fnox.toml (see Secrets below)
cargo test                                     # runs default-members only
cargo test --workspace                         # runs all crates
cargo test -- --nocapture

# Run a single test
cargo test test_name
cargo test -p baml-rt test_name               # specific crate
cargo test -p baml-rt-a2a test_name -- --nocapture

# Feature-gated test suites
cargo test -p baml-rt --features llm-tests -j 1

# Lint (run before committing)
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check

# Full pre-commit checks (fmt, clippy, regen-fixtures when relevant paths change, typos, cargo-check)
pre-commit run --all-files

# Snapshot testing (provenance crate uses insta)
cargo insta review

# Binaries
cargo run -p baml-rt-builder --bin baml-agent-builder   # lint, compile, package; `publish` → repository + deploy
cargo run -p baml-agent-runner                           # HTTP A2A + embedded /repository; agents via publish + POST /deploy (options only, no positional tar paths)
cargo run -p cargo-agent-platform                        # MCP server management, agent chat interface

# Nextest (CI-style: one run, JUnit)
cargo install cargo-nextest        # once
./scripts/nextest-ci-local.sh      # full workspace + http-tools; JUnit at target/nextest/ci/junit.xml

# Load testing scripts
python3 scripts/measure_a2a_sse.py --package PACKAGE --text "message"           # single SSE stream timing
python3 scripts/concurrent_a2a_sse.py --package PACKAGE --concurrency N         # parallel SSE streams

# E2E testing
just e2e-k8s                                   # full k3d cluster e2e harness
just e2e-k8s-cgroup-throttle                  # adversarial cgroup-throttled deploy fixture

# MCP demo (meteo weather server)
scripts/meteo_mcp.sh runner                   # start runner with MCP registry
scripts/meteo_mcp.sh chat                     # deploy and chat with meteo agent
```

### Secrets

API keys for tests are resolved through `fnox.toml` via `FnoxFileSecretResolver`. The file maps secret names to values with a `default` field. CI writes this file from GitHub secrets; locally, create `fnox.toml` in the project root.

The test model for LLM tests is controlled by the `BAML_TEST_MODEL` environment variable, which defaults to `x-ai/grok-4.3`. This can be overridden to use different models for testing.

## Local Setup (Linux)

Local release builds — `just coordinator-claude-notion`, `just dev-all-agents`, `cargo test` paths that link the runner binary — need three system-level dependencies that the runner Docker image installs but a fresh Linux host does not:

```bash
sudo apt install -y libdbus-1-dev libcap-ng-dev pkg-config
npm install -g typescript@6
```

- `libdbus-1-dev` — pulled in via `fnox` → `keyring` → `dbus-secret-service` (the secret-resolver chain).
- `libcap-ng-dev` — pulled in via `microsandbox` for syscall capability filtering at the runner sandbox boundary.
- `typescript@6` — required on `PATH` for the agent build pipeline; the canonical `tsconfig.json` uses `moduleResolution: "bundler"` with TypeScript 6.x.

Run `just check-host` to verify all three are present before kicking off a release build. The recipe exits non-zero with a clear "missing X (install with Y)" message; on non-Linux hosts the Linux-only checks are skipped.

The runner Docker image installs these globally, so the cluster sections (`just e2e-k8s`, `scripts/k8s-pilot-*`) work without host-level installs. This friction is local-only.

## Architecture

Agentium OS is a Rust workspace (edition 2024, nightly pinned via `rust-toolchain.toml`) for executing BAML functions, running JavaScript agents via QuickJS, tool orchestration, and serving A2A (agent-to-agent) protocol requests.

**Agent authoring:** `docs/how-to-write-agents.md` (entrypoints, tools, plans + ReAct, citations, `StructuredReply`).

### Crate Map

**Foundation**
- **baml-rt-core** — Shared error types, result types, correlation helpers, event bus with effect subscriber observability, step executor outcomes, ID types
- **baml-rt-id** — Newtype ID wrappers (UUID-based)
- **baml-rt-hash** — Canonical content-addressable hashing for agent source bundles
- **baml-rt-config** — Tool configuration storage and resolution
- **baml-rt-llm-config** — Centralised LLM client configuration; `FnoxFileSecretResolver` for API keys; test model configuration via `test_model_default()` helper
- **baml-rt-vocabulary** — Vocabulary types (minimal)
- **baml-rt-embedding** — Embedding and drift detection

**Runtime**
- **baml-rt-tools** — Tool trait, registry/executor, session FSM (`ToolSessionPlan` with Open/Send/Read/Finish/Abort ops)
- **baml-rt-mcp** — MCP client and importer for the BAML runtime
- **baml-rt-interceptor** — Interceptor trait + pipeline (pre/post execution hooks)
- **baml-rt-observability** — OpenTelemetry tracing setup, spans, metrics; `init_tracing()` uses per-layer filters (`RUST_LOG_FMT`, `RUST_LOG_OTEL`); central `spans.rs` keeps A2A ingress at `info` and agent execution spans at `debug` (see `docs/otel-trace-instrumentation-guide.md`); exported OTLP metric names: `docs/metrics-inventory.md`; runner identity foundation for K8s pilot with service.instance.id and deployment.environment resource attributes; distributed tracing support for cross-pod A2A forwarding
- **baml-rt-quickjs** — QuickJS runtime host: loads JS, bridges JS↔Rust, manages BAML runtime invocations, provenance error mapping
- **baml-rt-a2a** — Agent-to-agent protocol: JSON-RPC types, SSE streaming transport, streaming task handling, task update broadcasting and session management
- **baml-rt-conversation** — Agent-visible conversation history projection and episode types; pure computation (no I/O); see `docs/agent-conversation-crate.md` and normative spec `docs/baml-rt-conversation-spec.md`
- **baml-rt-provenance** — Provenance graph: event normalization, SurrealDB persistence, cluster-safe archive refs with activity-anchor idempotency, effect subscriber observability, metamodel query system for graph traversal
- **baml-rt-repository** — Agent package repository: content-addressable archive with lineage, versioning, and search; MCP server registry and schema storage
- **baml-rt-router** — Cluster routing, SSRF validation, token auth, cross-pod A2A forwarding with distributed trace propagation
- **baml-rt-api** — HTTP API surface: agent discovery (GET /agents), A2A JSON-RPC forwarding, OpenAPI via utoipa, RFC 7807 errors, operator auth boundary, OpenTelemetry middleware for distributed tracing, conversation history endpoints, context metrics, runtime-progress-gated readiness probe, cluster-wide deployment fan-out (POST /cluster/deploy)

**Derive macros**
- **baml-derive-core** — Core types and rendering for derive macro (`BamlType` trait)
- **baml-derive** — Proc-macro: `#[derive(BamlType)]` with `#[baml(dynamic)]`, `#[baml(union)]`, `#[baml(alias)]`, `#[baml(skip)]` etc.
- **baml-tool-derive** — Proc-macro attribute for registering tool metadata and handlers
- **baml-derive-tests** — Integration tests for derive macros (not published)

**Integration clients** (`crates/integrations/`)
- **clickup-client** — Shared ClickUp API client for tools and daemons
- **github-client** — Shared GitHub REST API client
- **notion-read** — Shared Notion read-only API client
- **slack-read** — Shared Slack read-only Web API client

**Tool bundles** (`crates/tools/`)
- **tools/calculator** — Calculator support tool
- **tools/claude** — Claude tool bundle (streaming session)
- **tools/clickup** — ClickUp host tool
- **tools/notion** — Notion host tool
- **tools/slack** — Slack host tool (read-only)
- **tools/system** — System tool bundle (e.g. agent-to-agent calls)
- **tools/memory** — Persistent graph-based cognitive memory
- **tools/internal-dev** — Test tool implementations (Calculator, Delay, Uppercase, Weather, A2aRelay) registered via `inventory`

**Top-level binaries and facades**
- **baml-rt** — Facade crate re-exporting subcrates via feature flags (default: all enabled)
- **baml-rt-builder** — Agent build pipeline: BAML type generation, tar.gz packaging. Binary: `baml-agent-builder`
- **baml-agent-runner** — A2A host (stdio and/or HTTP); embedded agent repository and deploy-by-hash; deployment restore, conversation history/metrics. Binary: `baml-agent-runner`
- **cargo-agent-platform** — MCP server management CLI and agent chat interface
- **task-daemon** — Local polling daemon substrate for extracting actionable tasks from sources (Slack, etc.)

**Test**
- **test-support** — Shared test fixtures and helpers (not published)

### Key Runtime Flow

**Conversational (A2A):** JS code → `QuickJSBridge` → checks `globalThis` for JS function → if missing, falls back to `BamlRuntimeManager` → runs interceptor pipeline → calls BAML runtime → LLM provider → tool session execution (host tools run in Rust, never JS) → interceptor post-hooks → result back to JS as resolved Promise.

**Dispatch (event delivery):** `AgentDispatchRequest` → `POST /agents/{pkg}/{inst}/dispatch` → A2A transport → calls `onDispatch` on `globalThis` → agent returns `AgentDispatchAck`. No conversational context; used for host-to-agent event delivery from sources like task-daemon.

### MCP Integration

MCP (Model Context Protocol) servers provide external tools and resources to agents. The integration supports stdio transport for local MCP servers.

**MCP Registry:** The repository stores MCP server schemas and tool definitions. Schemas are discovered via `cargo-agent-platform mcp enable` and stored in the registry for agent builds.

**Agent Build Integration:** Agents can reference MCP tools in their BAML schemas. The builder fetches schemas from the registry (via `BAML_MCP_REGISTRY_URL`) and generates TypeScript types for MCP tools.

**Runtime Execution:** MCP tools are executed through the standard tool session FSM. The runtime manages MCP server processes and handles stdio communication.

**Configuration:** MCP servers are configured in `~/.agentium-os/mcp-servers.json` with command, args, and environment variables. The registry stores schema snapshots separately from runtime configuration.

### Host Tool Contract

Host tools are session-based. BAML returns a declarative tool session fragment (wrapper `step` or flat `op`) with Open → Send / Read → Finish/Abort. The Rust runtime executes each fragment; JavaScript never mediates host tool execution except via `openToolSession` helpers.

Tools have two roles: **invoke** (agent calls tool via session FSM) and optionally **produce events** (tool declares `event_sources` in metadata, host polls and routes to subscribed agents). See `docs/host-to-agent-event-delivery.md` for the full model.

**BAML return shape and session plans:** A BAML result that looks like a tool session fragment—top-level **`step`** with `op`, or a flat object with **`op`**—is parsed and executed as one FSM hop (`Open` / `Send` / `Read` / `Finish` / `Abort`). Coordinator *product* plans (ordered work for your loop) must **not** reuse that shape at the top level; use distinct fields (e.g. `plan_steps`). Session-planning BAML functions should be listed in builder-generated `session_plan_functions.json`. See `docs/how-to-write-agents.md` §3.

### Conversation Handling (A2A DSL) — Reference Example

The **best example** of multi-turn conversation and task lifecycle is the **task-lifecycle-demo** fixture: `tests/fixtures/agents/task-lifecycle-demo/src/index.ts`.

- **Entrypoint:** `__chat_register({ run })` — the agent implements `run(ctx: RunContext)`; the runtime wraps it into `onChatMessage`. No boilerplate `session(message).run(...)` in agent code.
- **Context:** `ctx.text` (first text part), `ctx.message` (inbound message), `ctx.emit` (message, artifact, `awaitInput`).
- **Suspension:** `await ctx.emit.awaitInput(prompt)` emits INPUT_REQUIRED and resumes when the next message is routed to the same task/context.
- **Helpers:** `messageText(message)` for any message; `session(message).text()` for the initial message. Messages from `awaitInput` have `.text()`.
- **Flow:** Path choice → review loop → sign-off loop → COMPLETED (sequential loops, no nesting).

Other fixtures (stream-js-tool, stream-baml-tool, conversational-context-auto, etc.) use the same DSL; task-lifecycle-demo is the most complete reference.

**Dispatch (event delivery):** `dispatch-echo` fixture (`tests/fixtures/agents/dispatch-echo/`) is the minimal example of `onDispatch` handling. Agents declare subscriptions in `manifest.json` under `discovery.subscriptions` to receive events. See `docs/host-to-agent-event-delivery.md`.

### HTTP API Authentication

The runner HTTP API has two access tiers:

**Public routes** (no authentication required):
- `GET /healthz`, `GET /readyz` — health checks (readiness gated on runtime-progress meter)
- `GET /agents` — agent discovery
- `POST /agents/{pkg}/{inst}/chat` — A2A JSON-RPC forwarding
- `POST /agents/{pkg}/{inst}/dispatch` — host-to-agent event delivery
- `GET /contexts/{id}/*`, `GET /tasks/{id}/episode*`, conversation-history and provenance reads — observability over the provenance graph

**Operator routes** (require `X-Runner-Token` header in cluster mode):
- `GET /config`, `POST /config` — configuration management
- `GET /config/secrets-overview` — secrets inventory
- `POST /deploy`, `POST /undeploy` — deployment lifecycle
- `POST /cluster/deploy` — cluster-wide deployment fan-out (resolves hash once, forwards to all runners)
- `POST /migrate` — database migration
- Repository mutation endpoints

The runner token is configured via the `RUNNER_TOKEN` environment variable or `runner-token` Kubernetes secret; operator routes reject requests without a valid token in cluster mode. Public routes (`/chat`, `/dispatch`, `/agents`, health probes) are reachable from any pod on the cluster network — the runner NetworkPolicy does not fence them, though SurrealDB ingress is restricted to runner pods. The trust boundary for cross-pod A2A is therefore the cluster network perimeter plus operator-route token gating, not the runner NetworkPolicy.

### Readiness Probe Contract

`GET /readyz` returns `200` when both the boot latch is set (event producers registered) and the runtime-progress meter indicates the system is within the readiness threshold (lag below 1000ms). The meter aggregates the tokio ticker and QuickJS event-loop probe, so stalls on either side flip the gate to `503`. This prevents kubelet from routing traffic to pods with stalled runtimes (cgroup-throttled deploys, wedged QuickJS loops, deadlocked tasks).

### Feature Flags (baml-rt facade)

- `tools` → baml-rt-tools
- `interceptor` → baml-rt-interceptor
- `observability` → baml-rt-observability
- `quickjs` → baml-rt-quickjs (implies tools + interceptor + observability)
- `a2a` → baml-rt-a2a (implies quickjs)
- `builder` → baml-rt-builder (implies observability)

### Test-Gating Feature Flags

- `llm-tests` — LLM-dependent tests requiring API keys (on baml-rt, baml-agent-runner, task-daemon)
- `http-tools` — HTTP-dependent tools (ClickUp, Notion, Slack) plus **security-eval** mock tools (`support/crm`, `support/email`) on `baml-rt-builder` and `baml-agent-runner`. **`baml-agent-builder package`** and **`regen_fixtures`** need these features when the manifest lists those tools (use `just build-release` / `just regen-fixtures`, or `--all-features`).

## CI Structure

Single job in `rust-ci.yml` (push/PR to main, plus manual dispatch):

- **nextest (workspace)** — `cargo nextest run --workspace --locked --profile ci` with all feature flags enabled (`http-tools`, `llm-tests`, `memory`). Uses rust-cache with shared key `ci-nextest`. JUnit report published via `mikepenz/action-junit-report`. Secrets written to `fnox.toml` from GitHub secrets. **`regen_fixtures` is not run in CI**; generated `agents/**` and `tests/fixtures/agents/**` outputs stay committed, refreshed locally via `just regen-fixtures` / the pre-commit `regen-fixtures` hook.
- Toolchain: stable for build/test, nightly for `cargo fmt --check` only.
- **APT reliability:** CI uses `scripts/ci/apt-update-retry.sh` to mitigate transient Ubuntu mirror sync failures during package installation.
- **TypeScript 6.x:** CI installs `typescript@6` globally to match the runner image and ensure consistent fixture builds. The canonical `tsconfig.json` uses `moduleResolution: "bundler"` with TypeScript 6.x.
- **Test model configuration:** CI sets `BAML_TEST_MODEL` from the `vars.BAML_TEST_MODEL` repository variable with fallback to `x-ai/grok-4.3` for backward compatibility.

## Testing Conventions

- **Vertical slices over unit shards**: test via public API surfaces (`BamlRuntimeManager`, `QuickJSBridge`, `A2aRequestHandler`), not internal shortcuts
- **test-support crate**: use `setup_baml_runtime_default()`, `setup_baml_runtime_from_fixture()`, `setup_bridge()`, `agent_fixture()`, `require_api_key()`, `ensure_fixture_runtime_types()`, `ensure_baml_src_exists()`
- **Call `ensure_fixture_runtime_types()`** at the start of any E2E test loading from `tests/fixtures/agents/`
- **Async tests**: use `#[tokio::test]`
- **Snapshot tests**: `insta::assert_json_snapshot!` in provenance crate; update with `cargo insta review`
- **Property tests**: scope attribution, tool session lifecycle, stream ordering (using proptest)
- **Test fixtures**: `tests/fixtures/agents/` (agent packages), `baml_src/` (BAML schemas)
- **E2E testing**: `just e2e-k8s` runs the full k3d cluster harness; `just e2e-k8s-cgroup-throttle` runs adversarial cgroup-throttled deploy fixture testing runner readiness under CPU constraints with relaxed readiness probe expectations (accepts both `200` and `503` responses during CPU starvation, defending transport-level liveness rather than gate verdict) (see `docs/testing/e2e-k8s.md`)

## Rust Conventions

- Use `dotenvy` (not dotenv)
- Use named string interpolation: `format!("{name} is {value}")` not `format!("{} is {}", name, value)`
- Never unwrap in production code; use `?` with proper error types
- Never silently discard errors with `let _ =` without logging
- Error variant names should describe the operation that failed (e.g., `VaultRetrieval`, not `External`)
- **`Option` is a type of last resort, not a default.** `Option` means "this value may be legitimately absent at this point in the program." It does not mean "I haven't built it yet" (use typestate), "it depends on the variant" (use an enum with different fields per variant), or "the DashMap might not have it" (fix the insertion guarantee or assert). Specifically:
  - Model construction order with **typestate** (e.g., `Raw` → `Hydrated`), not `Option` bags where fields start as `None` and are filled in later.
  - Model variant-specific data with **discriminated unions** (enum variants with different field sets), not flat structs with `Option` fields that are "only valid when op_kind is X."
  - `DashMap::remove().map()` producing `Option` for a value that was structurally guaranteed to be inserted is a bug — fix the insertion or make the removal an assertion.
  - Silent `None → skip` in write paths is prohibited — it produces invisible graph corruption far from the source. Use hard errors or logged degradation with a synthetic fallback.
- Use newtype wrappers for domain IDs and values
- Structured logging: static messages with dynamic data in fields (`tracing::info!(from = %from, event = "payment")`)
- No version history in comments; describe current behavior in present tense
- `#[allow(dead_code)]` requires a justifying comment explaining why the code is reserved
- **Graph-first provenance reads:** All provenance read paths (conversation_context, episode assembly, drift scoring, graph export) must reconstruct data by traversing graph edges — never by parsing node ID prefixes, matching timestamps, or building HashMap joins from string keys. If a read path needs a relationship that isn't expressed as an edge, fix the write path. The edge table stores `from_label` and `to_label` — use them instead of string prefix matching on `from_id`/`to_id`. Ephemeral per-conversation indices (`@N`, `#N`) must be resolved at write time and not stored on graph edges.

## Deployment

### Supported Install Surface

The Helm chart at `deploy/helm/agentium-os/` is the supported Kubernetes install path. It packages the pilot topology (two-runner StatefulSet + shared SurrealDB) as a single `helm upgrade --install` command:

```bash
helm upgrade --install agentium deploy/helm/agentium-os/ \
  --namespace agentium --create-namespace \
  -f deploy/helm/agentium-os/examples/design-partner-values.yaml
```

Prerequisites include creating secrets and ConfigMaps before installation:
- `surrealdb-credentials` secret (username/password)
- `runner-token` secret (operator authentication)
- `fnox-config` ConfigMap (LLM configuration)

The chart supports both design partner values (production-like) and k3d values (local development). There is no published runner image — operators must build and push their own.

The chart includes observability configuration for OpenTelemetry pilot deployments. When `observability.enabled` is true, the runner pods receive OTEL_* environment variables with the pilot identity contract: `service.name=agentium-runner`, `service.instance.id=$POD_NAME`, `k8s.namespace.name=$POD_NAMESPACE`, and `deployment.environment` (defaults to `global.environment`). The `observability.otlpEndpoint` setting configures the OTLP gRPC collector endpoint; when empty, OTLP export remains disabled.

See `deploy/helm/agentium-os/README.md` for complete installation instructions and verification steps.

### Demo / Legacy Manifests

The raw manifests under `deploy/k8s/` and the `deploy/demo/run-demo.sh` script are internal assets used for local k3d development and the e2e test harness. They are not the operator-facing install surface.

- `deploy/k8s/runner.yaml` — Runner StatefulSet (demo image, local-only)
- `deploy/k8s/surrealdb.yaml` — SurrealDB StatefulSet
- `deploy/k8s/networkpolicy.yaml` — Network isolation policies
- `deploy/k8s/runner-token.yaml.example` — Secret template
- `deploy/demo/run-demo.sh` — Local k3d cluster bootstrap

## External Dependencies

- **BAML runtime**: git dependency from `ryan-s-roberts/baml` (`canary` branch); `baml-runtime`, `baml-types`, `internal-baml-core`, `internal-llm-client`
- **QuickJS**: `quickjs_runtime` crate for JS execution
- **SurrealDB**: Embedded multi-model database for provenance graph persistence
- **MCP SDK**: `rmcp` crate for Model Context Protocol client implementation with stdio transport
- **TypeScript 6.x**: required on `PATH` (or via `npx`) for any code path that exercises the agent build pipeline — `cargo test` on builder fixtures, local `baml-agent-builder package`, and the runner image (Dockerfile installs `typescript@6` globally). The canonical `tsconfig.json` written by bootstrap uses `moduleResolution: "bundler"`. Install with `npm install -g typescript@6`.
