set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set dotenv-load

provenance_db := "provenance.db"
# Separate SurrealKV store dirs (provenance + sibling config.db) so this stack can run alongside another runner using `provenance.db`.
provenance_persona_claude_notion_db := "persona-claude-notion-provenance.db"
runner_http_bind := "127.0.0.1:8080"
slack_channel := "agentium-eng"

# Binaries (build once with `just build-release`, then agent recipes use these).
# Respect CARGO_TARGET_DIR when present (.env sets it in some dev setups).
builder_bin := "${CARGO_TARGET_DIR:-target}/debug/baml-agent-builder"
runner_bin := "${CARGO_TARGET_DIR:-target}/debug/baml-agent-runner"
graph_exporter_bin := "${CARGO_TARGET_DIR:-target}/debug/graph_exporter"

# Regenerate `_baml_runtime.baml` + `src/baml-runtime.d.ts` for every fixture under `tests/fixtures/agents/` and agent under `agents/`. Requires all tool crates (same as build-release).
regen-fixtures:
    cargo run -p baml-rt-builder --all-features --bin regen_fixtures

# Build release versions of builder, runner, and graph_exporter. Run once before using agent recipes.
build-release:
    cargo build -p baml-rt-builder --bin baml-agent-builder --all-features
    cargo build -p baml-agent-runner --all-features
    cargo build -p baml-rt-provenance --bin graph_exporter --features cli

# Build the runner in debug mode (fast local iteration).
build:
    cargo build -p baml-agent-runner --all-features

# Rebuilds clickup-agent package and runs it via a2a stdio. Requires: just build-release
clickup-agent: build-release
    {{builder_bin}} package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    {{runner_bin}} clickup-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as clickup-agent, but persists provenance to provenance.db for graph_exporter.
clickup-agent-provenance: build-release
    {{builder_bin}} package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    {{runner_bin}} clickup-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

# Rebuilds notion-agent package and runs it via a2a stdio. Requires: just build-release
notion-agent: build-release
    {{builder_bin}} package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    {{runner_bin}} notion-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as notion-agent, but persists provenance to provenance.db for graph_exporter.
notion-agent-provenance: build-release
    {{builder_bin}} package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    {{runner_bin}} notion-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

# Rebuilds slack-agent package and runs it via a2a stdio.
slack-agent:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/slack-agent --output slack-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- slack-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as slack-agent, but persists provenance to provenance.db for graph_exporter.
slack-agent-provenance:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/slack-agent --output slack-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- slack-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

# Rebuilds coordinator + notion + clickup packages and runs coordinator-agent via a2a stdio. Requires: just build-release
coordinator-agent: build-release
    {{builder_bin}} package --agent-dir agents/coordinator-agent --output coordinator-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    {{runner_bin}} coordinator-agent.tar.gz notion-agent.tar.gz clickup-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as coordinator-agent, but persists provenance to provenance.db for graph_exporter.
coordinator-agent-provenance: build-release
    {{builder_bin}} package --agent-dir agents/coordinator-agent --output coordinator-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{builder_bin}} package --agent-dir tests/fixtures/agents/conversational-persona-demo --output conversational-persona-demo.tar.gz
    {{runner_bin}} coordinator-agent.tar.gz notion-agent.tar.gz clickup-agent.tar.gz claude-session-agent.tar.gz conversational-persona-demo.tar.gz --a2a-stdio --provenance-db {{provenance_db}} --serve-http {{runner_http_bind}}

# Rebuilds claude-session-agent package and runs it via a2a stdio. Requires: just build-release
claude-session-agent: build-release
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{runner_bin}} claude-session-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as claude-session-agent, but persists provenance to provenance.db for graph_exporter.
claude-session-agent-provenance: build-release
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{runner_bin}} claude-session-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

# Rebuilds persona + notion and runs with provenance and UI (HTTP only).
persona-notion: build-release
    {{builder_bin}} package --agent-dir tests/fixtures/agents/conversational-persona-demo --output conversational-persona-demo.tar.gz
    {{builder_bin}} package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    {{runner_bin}} conversational-persona-demo.tar.gz notion-agent.tar.gz --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}} --web-dir web/dist

# Persona + Claude session + Notion, HTTP + provenance + web UI. Requires web/dist (`cd web && npm ci && npm run build`).
persona-claude-notion: build-release
    {{builder_bin}} package --agent-dir tests/fixtures/agents/conversational-persona-demo --output conversational-persona-demo.tar.gz
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    {{runner_bin}} conversational-persona-demo.tar.gz claude-session-agent.tar.gz notion-agent.tar.gz --serve-http {{runner_http_bind}} --provenance-db {{provenance_persona_claude_notion_db}} --web-dir web/dist

# Full local dev stack: all primary dev agent packages, web UI, provenance.
# `.env` is loaded via `set dotenv-load`. HTTP only (no --a2a-stdio) so the server stays up without a stdio client.
# Requires: web/dist (cd web && npm ci && npm run build).
dev-all-agents: build-release
    {{builder_bin}} package --agent-dir tests/fixtures/agents/conversational-persona-demo --output conversational-persona-demo.tar.gz
    {{builder_bin}} package --agent-dir tests/fixtures/agents/security-eval-agent --output security-eval-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/coordinator-agent --output coordinator-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/extrospection-agent --output extrospection-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/slack-agent --output slack-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/workflow-intake-agent --output workflow-intake-agent.tar.gz
    {{runner_bin}} conversational-persona-demo.tar.gz claude-session-agent.tar.gz clickup-agent.tar.gz coordinator-agent.tar.gz extrospection-agent.tar.gz notion-agent.tar.gz security-eval-agent.tar.gz slack-agent.tar.gz workflow-intake-agent.tar.gz --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}} --web-dir web/dist

# Rebuilds persona + claude-session + extrospection + clickup + security-eval and runs them with provenance (HTTP only, no stdio).
persona-claude-extrospection-clickup: build-release
    {{builder_bin}} package --agent-dir tests/fixtures/agents/conversational-persona-demo --output conversational-persona-demo.tar.gz
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/extrospection-agent --output extrospection-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    {{builder_bin}} package --agent-dir tests/fixtures/agents/security-eval-agent --output security-eval-agent.tar.gz
    {{runner_bin}} conversational-persona-demo.tar.gz claude-session-agent.tar.gz extrospection-agent.tar.gz clickup-agent.tar.gz security-eval-agent.tar.gz --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

# Rebuilds persona + claude-session + extrospection + security-eval packages and runs them via a2a stdio.
persona-claude-extrospection: build-release
    {{builder_bin}} package --agent-dir tests/fixtures/agents/conversational-persona-demo --output conversational-persona-demo.tar.gz
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/extrospection-agent --output extrospection-agent.tar.gz
    {{builder_bin}} package --agent-dir tests/fixtures/agents/security-eval-agent --output security-eval-agent.tar.gz
    {{runner_bin}} conversational-persona-demo.tar.gz claude-session-agent.tar.gz extrospection-agent.tar.gz security-eval-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as persona-claude-extrospection, but persists provenance to provenance.db.
persona-claude-extrospection-provenance: build-release
    {{builder_bin}} package --agent-dir tests/fixtures/agents/conversational-persona-demo --output conversational-persona-demo.tar.gz
    {{builder_bin}} package --agent-dir agents/claude-session-demo --output claude-session-agent.tar.gz
    {{builder_bin}} package --agent-dir agents/extrospection-agent --output extrospection-agent.tar.gz
    {{builder_bin}} package --agent-dir tests/fixtures/agents/security-eval-agent --output security-eval-agent.tar.gz
    {{runner_bin}} conversational-persona-demo.tar.gz claude-session-agent.tar.gz extrospection-agent.tar.gz security-eval-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}



# Runs the HTTP Notion demo script (starts runner if needed and streams one request).
notion-demo:
    ./scripts/run-notion-demo.sh

# Stops the background runner started by notion-demo.
notion-demo-stop:
    ./scripts/stop-notion-demo.sh

# Runs the HTTP Slack todo-extraction demo script (starts runner if needed and streams one request).
slack-demo:
    ./scripts/run-slack-demo.sh

# Stops the background runner started by slack-demo.
slack-demo-stop:
    ./scripts/stop-slack-demo.sh

# Runs one polling cycle of the local Slack task daemon.
task-daemon-slack-once:
    cargo run -p baml-task-daemon -- run --channel {{slack_channel}} --once

# Runs the local Slack task daemon in watch mode.
task-daemon-slack:
    cargo run -p baml-task-daemon -- run --channel {{slack_channel}} --interval-seconds 120

# Runs the leadership-oriented task-daemon demo flow (Slack -> LLM -> coordinator -> provenance).
task-daemon-demo:
    ./scripts/run-task-daemon-demo.sh

# Stops the coordinator backend used by task-daemon-demo.
task-daemon-demo-stop:
    ./scripts/stop-coordinator-demo.sh

# Runs coordinator + notion HTTP demo and streams one coordinated request.
coordinator-demo:
    ./scripts/run-coordinator-demo.sh

# Stops the background runner started by coordinator-demo.
coordinator-demo-stop:
    ./scripts/stop-coordinator-demo.sh

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

ci_features := "baml-rt-builder/http-tools,baml-rt-builder/llm-tests,baml-agent-runner/http-tools,baml-agent-runner/memory,baml-rt/llm-tests,baml-agent-runner/llm-tests"

# CI parity: run nextest in CI order (LLM suite first, then non-LLM suite).
# Requires: cargo-nextest and OPENROUTER_API_KEY for LLM tests.
# LLM suite: tests gated by #[cfg(feature = "llm-tests")] compile and run with -j 2.
# Non-LLM suite: no llm-tests feature, higher parallelism.
test:
    cargo nextest run --workspace --locked --profile ci-llm --no-fail-fast -j 2 --features baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-rt-builder/llm-tests,baml-agent-runner/http-tools,baml-agent-runner/memory,baml-rt/llm-tests,baml-agent-runner/llm-tests
    THREADS=$(( $(nproc) / 2 )); [ "$THREADS" -lt 2 ] && THREADS=2; cargo nextest run --workspace --locked --profile ci-non-llm --no-fail-fast -j "$THREADS" --features baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-agent-runner/http-tools,baml-agent-runner/memory

# Same as `test` but only compile — useful for a quick pre-push check.
test-build:
    cargo nextest run --workspace --features {{ci_features}} --no-run

# Run only a single crate's tests (e.g. `just test-crate baml-rt-provenance`).
test-crate crate:
    cargo nextest run -p {{crate}} --features {{ci_features}}

# Run tests that don't need FalkorDB or API keys (unit tests only).
test-unit:
    cargo nextest run --workspace --features baml-rt-builder/http-tools,baml-agent-runner/http-tools,baml-agent-runner/memory

# Export a Mermaid sequence diagram for a given context-id. Requires: just build-release
# Usage: just provenance-mermaid ctx-1771426017780-2
provenance-mermaid context_id: build-release
    {{graph_exporter_bin}} --db {{provenance_db}} --context-id {{context_id}} --simplify --format mermaid

# SDK CLI: workspace integrity check
doctor:
    cargo run -p cargo-agent-platform -- doctor

# SDK CLI: list all registered tools
list-tools:
    cargo run -p cargo-agent-platform -- list-tools

# SDK CLI: list all agent packages
list-agents:
    cargo run -p cargo-agent-platform -- list-agents
