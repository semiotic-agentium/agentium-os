set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
# Loads `.env` next to this justfile for recipes that use the default shell (plain `cargo` lines).
set dotenv-load

# Shebang recipes additionally `cd` to the repo root and run `set -a; [ -f .env ] && . ./.env; set +a`
# so assignments in `.env` are exported (same idea as `source .env` in bash).

provenance_db := "provenance.db"
# Separate SurrealKV store dirs (provenance + sibling config.db) so this stack can run alongside another runner using `provenance.db`.
provenance_persona_claude_notion_db := "persona-claude-notion-provenance.db"
runner_http_bind := "127.0.0.1:8081"
# Runner: options only — no positional packages. Agents load via `baml-agent-builder publish` + POST /deploy.
runner_base_url := "http://" + runner_http_bind
repository_url := runner_base_url + "/repository"
# Embedded SurrealKV paths (explicit; same defaults the runner would use relative to cwd).
runner_state_dir := ".runner-state"
runner_repository_dir := ".repository"
slack_channel := "agentium-eng"

# Binaries (build once with `just build-release`, then agent recipes use these).
# Respect CARGO_TARGET_DIR when present (.env sets it in some dev setups).
builder_bin := "${CARGO_TARGET_DIR:-target}/debug/baml-agent-builder"
runner_bin := "${CARGO_TARGET_DIR:-target}/debug/baml-agent-runner"
graph_exporter_bin := "${CARGO_TARGET_DIR:-target}/debug/graph_exporter"

# Regenerate `_baml_runtime.baml` + `src/baml-runtime.d.ts` for every fixture under `tests/fixtures/agents/` and agent under `agents/`. Requires all tool crates (same as build-release).
regen-fixtures:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo run -p baml-rt-builder --all-features --bin regen_fixtures

# Pre-download fastembed ONNX models to models/fastembed/ (git-LFS tracked).
# Run once after a fresh clone or when models/ is empty.
# Models are also committed via LFS — git lfs pull will restore them without re-downloading.
download-models:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo run -p baml-rt-embedding --bin download_models

# Build release versions of builder, runner, and graph_exporter. Run once before using agent recipes.
build-release:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo build -p baml-rt-builder --bin baml-agent-builder --all-features
    cargo build -p baml-agent-runner --all-features
    cargo build -p baml-rt-provenance --bin graph_exporter --features cli

# Build the runner in debug mode (fast local iteration).
build:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo build -p baml-agent-runner --all-features

# Build Vue/Vite SPA to web/dist (`npm ci` + `npm run build`). Required for recipes that pass `--web-dir web/dist`.
web-build:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cd web
    npm ci
    npm run build

# Rebuilds clickup-agent and runs it via a2a stdio. Deploys through the embedded repository (publish + POST /deploy).
# Requires: just build-release, curl
clickup-agent: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as clickup-agent, but persists provenance to provenance.db for graph_exporter.
clickup-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Rebuilds notion-agent and runs it via a2a stdio.
notion-agent: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as notion-agent, but persists provenance to provenance.db for graph_exporter.
notion-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Rebuilds slack-agent and runs it via a2a stdio. Requires: just build-release (http-tools via --all-features)
slack-agent: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/slack-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as slack-agent, but persists provenance to provenance.db for graph_exporter.
slack-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/slack-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Rebuilds coordinator + notion + clickup and runs coordinator stack via a2a stdio.
coordinator-agent: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as coordinator-agent, but adds claude-session + conversational-persona and persists provenance.
coordinator-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir tests/fixtures/agents/conversational-persona-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Rebuilds claude-session-agent and runs it via a2a stdio.
claude-session-agent: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as claude-session-agent, but persists provenance to provenance.db for graph_exporter.
claude-session-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Rebuilds persona + notion and runs with provenance and UI (HTTP only).
persona-notion: web-build build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --web-dir web/dist &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    for _ in $(seq 1 120); do
      if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
      sleep 0.5
    done
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/conversational-persona-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    wait "$runner_pid"

# Persona + Claude session + Notion, HTTP + provenance + web UI.
persona-claude-notion: web-build build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_persona_claude_notion_db}} --web-dir web/dist &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    for _ in $(seq 1 120); do
      if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
      sleep 0.5
    done
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/conversational-persona-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    wait "$runner_pid"

# Full local dev stack: all primary dev agent packages, web UI, provenance.
# HTTP only (no --a2a-stdio) so the server stays up without a stdio client.
dev-all-agents: web-build build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --web-dir web/dist &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    for _ in $(seq 1 120); do
      if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
      sleep 0.5
    done
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/conversational-persona-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/slack-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/workflow-intake-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    wait "$runner_pid"

# Rebuilds persona + claude-session + extrospection + clickup + security-eval and runs them with provenance (HTTP only, no stdio).
persona-claude-extrospection-clickup: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    for _ in $(seq 1 120); do
      if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
      sleep 0.5
    done
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/conversational-persona-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    wait "$runner_pid"

# Rebuilds persona + claude-session + extrospection + security-eval and runs them via a2a stdio.
persona-claude-extrospection: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir tests/fixtures/agents/conversational-persona-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as persona-claude-extrospection, but persists provenance to provenance.db.
persona-claude-extrospection-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then break; fi
        sleep 0.5
      done
      {{builder_bin}} publish --agent-dir tests/fixtures/agents/conversational-persona-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio



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
# Both suites use full parallelism (no thread limits).
test:
    cargo nextest run --workspace --locked --profile ci-llm --no-fail-fast --features baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-rt-builder/llm-tests,baml-agent-runner/http-tools,baml-agent-runner/memory,baml-rt/llm-tests,baml-agent-runner/llm-tests
    cargo nextest run --workspace --locked --profile ci-non-llm --no-fail-fast --features baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-agent-runner/http-tools,baml-agent-runner/memory

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
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
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
