set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
# Loads `.env` next to this justfile for recipes that use the default shell (plain `cargo` lines).
set dotenv-load

# Shebang recipes additionally `cd` to the repo root and run `set -a; [ -f .env ] && . ./.env; set +a`
# so assignments in `.env` are exported (same idea as `source .env` in bash).

provenance_db := "provenance.db"
# Separate SurrealKV store dirs (provenance + sibling config.db) so this stack can run alongside another runner using `provenance.db`.
provenance_persona_claude_notion_db := "persona-claude-notion-provenance.db"
# Default HTTP bind for `just` recipes. Port 8080 is a frequent conflict (e.g. inference gateways that 503
# every route until a huge model loads). To use 8080, change the assignment below to `127.0.0.1:8080`.
runner_http_bind := "127.0.0.1:18080"
# Runner: options only — no positional packages. Agents load via `baml-agent-builder publish` + POST /deploy.
runner_base_url := "http://" + runner_http_bind
repository_url := runner_base_url + "/repository"
# Embedded SurrealKV paths (explicit; same defaults the runner would use relative to cwd).
runner_state_dir := ".runner-state"
runner_repository_dir := ".repository"
slack_channel := "agentium-eng"
# OTEL defaults for local dev visor (collector from ./observability/docker-compose.yml).
# Safe defaults in code: if these are unset, OTLP export stays disabled.
otel_endpoint := "http://localhost:4317"
otel_protocol := "grpc"

# Binaries (build with `just build` / `just build-release`; paths default to release).
# Respect CARGO_TARGET_DIR when present (.env sets it in some dev setups).
builder_bin := "${CARGO_TARGET_DIR:-target}/release/baml-agent-builder"
runner_bin := "${CARGO_TARGET_DIR:-target}/release/baml-agent-runner"
graph_exporter_bin := "${CARGO_TARGET_DIR:-target}/release/graph_exporter"

# Regenerate `_baml_runtime.baml` + `src/baml-runtime.d.ts` for every fixture under `tests/fixtures/agents/` and agent under `agents/`. Requires all tool crates (same as build-release).
regen-fixtures:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo run --release -p baml-rt-builder --all-features --bin regen_fixtures

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
    cargo run --release -p baml-rt-embedding --bin download_models

# Build release versions of builder, runner, and graph_exporter. Run once before using agent recipes.
build-release:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo build --release -p baml-rt-builder --bin baml-agent-builder --all-features
    cargo build --release -p baml-agent-runner --all-features
    cargo build --release -p baml-rt-provenance --bin graph_exporter --features cli

# Build the runner in release mode (default; matches `runner_bin`).
# Note: build does not require OTEL env vars; export wiring is runtime-only.
build:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo build --release -p baml-agent-runner --all-features

# Build the runner in debug mode (binary: target/debug/baml-agent-runner).
build-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo build -p baml-agent-runner --all-features

# Run bare runner (HTTP + stdio) with OTEL defaults suitable for local docker observability stack.
# Override by exporting OTEL_* in your shell or .env.
runner: build
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as `runner`, but persists provenance to provenance.db.
runner-provenance: build
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Profile runner CPU hotspots with flamegraph.
# Requires: cargo-flamegraph (`cargo install flamegraph`) and Linux perf permissions.
# Output: flamegraph.svg in repo root.
profile-runner:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo flamegraph --release -p baml-agent-runner --bin baml-agent-runner -- \
      --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

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

# Start local OpenTelemetry stack (Collector + Prometheus + Tempo + Grafana).
otel-up:
    ./scripts/otel-stack.sh up

# Stop local OpenTelemetry stack.
otel-down:
    ./scripts/otel-stack.sh down

# Show local OpenTelemetry stack status.
otel-ps:
    ./scripts/otel-stack.sh ps

# Tail local OpenTelemetry stack logs.
otel-logs:
    ./scripts/otel-stack.sh logs

# Print a text summary of top latency consumers from Prometheus metrics.
# Example: just otel-summary 15m
otel-summary window='30m':
    #!/usr/bin/env bash
    set -euo pipefail
    PROM_URL="${PROM_URL:-http://localhost:9090}"
    W='{{window}}'

    q() {
      local expr="$1"
      curl -sG "$PROM_URL/api/v1/query" --data-urlencode "query=$expr"
    }

    fmt_ms() {
      local v="${1:-0}"
      awk -v v="$v" 'BEGIN {
        if (v == "" || v == "null" || v == "NaN") { print "n/a"; exit }
        if (v >= 1000) { printf "%.2fs", v/1000.0; exit }
        printf "%.0fms", v
      }'
    }

    fmt_n() {
      local v="${1:-0}"
      awk -v v="$v" 'BEGIN {
        if (v == "" || v == "null" || v == "NaN") { print "n/a"; exit }
        printf "%.0f", v
      }'
    }

    echo "== OTEL summary (window: $W) =="
    echo

    echo "-- Total time split --"
    llm_total=$(q "sum(increase(baml_rt_llm_call_duration_ms_sum[$W]))" | jq -r '.data.result[0].value[1] // "0"')
    tool_total=$(q "sum(increase(baml_rt_tool_invocation_duration_ms_sum[$W]))" | jq -r '.data.result[0].value[1] // "0"')
    echo "LLM total:  $(fmt_ms "$llm_total")"
    echo "Tool total: $(fmt_ms "$tool_total")"
    echo

    echo "-- LLM total time by function (desc) --"
    while IFS=$'\t' read -r fn v; do
      [[ -z "${fn:-}" ]] && continue
      printf "%-45s %10s\n" "$fn" "$(fmt_ms "$v")"
    done < <(
      q "sort_desc(sum by (function) (increase(baml_rt_llm_call_duration_ms_sum[$W])))" \
        | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"'
    )
    echo

    echo "-- LLM average latency by function --"
    while IFS=$'\t' read -r fn v; do
      [[ -z "${fn:-}" ]] && continue
      printf "%-45s %10s\n" "$fn" "$(fmt_ms "$v")"
    done < <(
      q "(sum by (function) (increase(baml_rt_llm_call_duration_ms_sum[$W])) / sum by (function) (increase(baml_rt_llm_call_duration_ms_count[$W]))) and on (function) (sum by (function) (increase(baml_rt_llm_call_duration_ms_count[$W])) > 0)" \
        | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"'
    )
    echo

    echo "-- Tool total time by tool (desc) --"
    while IFS=$'\t' read -r tool v; do
      [[ -z "${tool:-}" ]] && continue
      printf "%-45s %10s\n" "$tool" "$(fmt_ms "$v")"
    done < <(
      q "sort_desc(sum by (tool) (increase(baml_rt_tool_invocation_duration_ms_sum[$W])))" \
        | jq -r '.data.result[]? | "\(.metric.tool // "unknown")\t\(.value[1])"'
    )
    echo

    echo "-- Tool calls by tool --"
    while IFS=$'\t' read -r tool v; do
      [[ -z "${tool:-}" ]] && continue
      printf "%-45s %10s\n" "$tool" "$(fmt_n "$v")"
    done < <(
      q "sum by (tool) (increase(baml_rt_tool_invocation_total[$W]))" \
        | jq -r '.data.result[]? | "\(.metric.tool // "unknown")\t\(.value[1])"'
    )

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



# Bootstraps and runs the sandbox echo example in foreground-runner mode:
# - exports env vars for external tools + bind allowlist
# - runs setup_bind_demo.sh (export rootfs + digest + metadata patch + validation)
# - prints push/chat commands for a second shell
# - starts runner with sandbox features enabled (foreground so logs are visible)
# Stop with Ctrl-C.
echo-sandbox-demo:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a

    export BAML_EXTERNAL_TOOLS_DIR="$PWD/examples/external-tools/dev_echo_sandbox"
    export BAML_SANDBOX_PROVIDER=microsandbox
    export BAML_SANDBOX_BIND_ROOTS="$PWD/.tmp"

    ./examples/external-tools/dev_echo_sandbox/setup_bind_demo.sh --image dev-echo-sandbox:local --force

    echo
    echo "Runner will start in foreground; auto-push will run after HTTP is ready."
    echo "Then in another shell, run chat:"
    echo "  cargo run -p cargo-agent-platform -- chat --agent echo-agent --url {{runner_base_url}}"
    echo

    (
      for _ in $(seq 1 120); do
        if curl -sf "{{runner_base_url}}/openapi.json" >/dev/null 2>&1; then
          break
        fi
        sleep 0.5
      done
      echo "[echo-sandbox-demo] pushing agents/echo-agent..."
      cargo run -p cargo-agent-platform -- push --agents agents/echo-agent --url {{runner_base_url}}
      echo "[echo-sandbox-demo] push completed."
    ) &

    exec cargo run -p baml-agent-runner --all-features -- --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

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
    cargo run --release -p baml-task-daemon -- run --channel {{slack_channel}} --once

# Runs the local Slack task daemon in watch mode.
task-daemon-slack:
    cargo run --release -p baml-task-daemon -- run --channel {{slack_channel}} --interval-seconds 120

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

# CI parity: single workspace pass with the union feature set, matching rust-ci.yml.
# Requires: cargo-nextest and OPENROUTER_API_KEY for LLM tests.
# OTEL is disabled for stability/noise reduction in local test runs.
test:
    OTEL_SDK_DISABLED=true OTEL_TRACES_EXPORTER=none OTEL_METRICS_EXPORTER=none OTEL_LOGS_EXPORTER=none OTEL_EXPORTER_OTLP_ENDPOINT="" cargo nextest run --workspace --locked --profile ci-merged --no-fail-fast --features baml-rt-tools/http-tools,{{ci_features}}

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
    cargo run --release -p cargo-agent-platform -- doctor

# SDK CLI: list all registered tools
list-tools:
    cargo run --release -p cargo-agent-platform -- list-tools

# SDK CLI: list all agent packages
list-agents:
    cargo run --release -p cargo-agent-platform -- list-agents

# Run E2E k8s tests against a real k3d cluster.
# Prerequisites: docker/podman, k3d, kubectl, jq, curl, Rust toolchain, Node.js.
# First run builds the Docker image (~5 min) and agent builder binary.
e2e-k8s:
    ./scripts/e2e-k8s/run.sh

# Run the Kubernetes pilot first-run smoke against an installed Helm release.
# Prerequisites: a running chart install (see docs/k8s-pilot-operator-guide.md)
# and an open port-forward, or pass `--port-forward` to have the script manage it.
k8s-pilot-smoke *args='':
    ./scripts/k8s-pilot-smoke.sh {{args}}

# Authoritative Kubernetes pilot package-validation flow.
# Mirrors docs/k8s-pilot-operator-guide.md end-to-end on a local k3d cluster:
# builds the runner image, creates the three required objects, installs the
# Helm chart, runs scripts/k8s-pilot-smoke.sh, and verifies runners register
# in cluster_runners. No post-install kubectl patches.
verify-k8s-pilot-package *args='':
    ./scripts/verify-k8s-pilot-package.sh {{args}}

# Load-test harness for the Kubernetes pilot.
# Brings up the observability stack + Helm-installed pilot (or reuses a
# running one with --skip-bringup), runs the three canonical #226
# scenarios (local_a2a, forwarded_a2a, split_dual_runner) at the committed
# workload defaults, and writes machine- and human-readable results under
# artifacts/load-test/<timestamp>/. See docs/k8s-pilot-load-testing.md.
k8s-load-test *args='':
    ./scripts/k8s-load-test.sh {{args}}
