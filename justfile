set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
# Loads `.env` next to this justfile for recipes that use the default shell (plain `cargo` lines).
set dotenv-load

# Shebang recipes additionally `cd` to the repo root and run `set -a; [ -f .env ] && . ./.env; set +a`
# so assignments in `.env` are exported (same idea as `source .env` in bash).

provenance_db := "provenance.db"
# Separate SurrealKV store dirs (provenance + sibling config.db) so this stack can run alongside another runner using `provenance.db`.
provenance_coordinator_claude_notion_db := "coordinator-claude-notion-provenance.db"
# Default HTTP bind for `just` recipes. Port 8080 is a frequent conflict (e.g. inference gateways that 503
# every route until a huge model loads). To use 8080, change the assignment below to `127.0.0.1:8080`.
runner_http_bind := "127.0.0.1:18080"
# Runner: options only — no positional packages. Agents load via `baml-agent-builder publish` + POST /deploy.
runner_base_url := "http://" + runner_http_bind
repository_url := runner_base_url + "/repository"
# Seconds to wait for runner HTTP before publish/push (runner init + large restore can exceed 60s).
runner_http_ready_secs := "180"
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

# Verify the local host has the system deps required to build agents.
# On non-Linux hosts, skips Linux-only checks (libdbus / libcap-ng);
# tsc 6.x is required everywhere; the canonical agent tsconfig uses modern
# `moduleResolution: "bundler"` (see `crates/baml-rt-builder/.../tsc.rs`). Exits non-zero with a clear message if any
# dep is missing or out of range. Run this before build-release on a fresh
# dev host.
check-host:
    #!/usr/bin/env bash
    set -euo pipefail
    missing=()
    ok=()
    if [[ "$(uname)" == "Linux" ]]; then
        if pkg-config --exists dbus-1 2>/dev/null; then
            ok+=("dbus-1:    $(pkg-config --modversion dbus-1)")
        else
            missing+=("libdbus-1-dev (sudo apt install libdbus-1-dev)")
        fi
        if pkg-config --exists libcap-ng 2>/dev/null; then
            ok+=("libcap-ng: $(pkg-config --modversion libcap-ng)")
        else
            missing+=("libcap-ng-dev (sudo apt install libcap-ng-dev)")
        fi
    fi
    if ! command -v tsc >/dev/null 2>&1; then
        missing+=("typescript@6 (install with: npm install -g typescript@6)")
    else
        tsc_version=$(tsc --version 2>/dev/null || true)
        tsc_major=""
        if [[ "$tsc_version" =~ ([0-9]+) ]]; then
            tsc_major="${BASH_REMATCH[1]}"
        fi
        if [[ -z "$tsc_major" || "$tsc_major" -lt 6 ]]; then
            missing+=("typescript@6 (found '${tsc_version:-unknown}'; install with: npm install -g typescript@6)")
        else
            ok+=("tsc:       $tsc_version")
        fi
    fi
    if [ "${#missing[@]}" -gt 0 ]; then
        echo "Missing host deps:" >&2
        for m in "${missing[@]}"; do echo "  - $m" >&2; done
        exit 1
    fi
    echo "Host deps OK:"
    printf '  %s\n' "${ok[@]}"

# Build release versions of builder and runner. Run once before using agent recipes.
build-release:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    cargo build --release -p baml-rt-builder --bin baml-agent-builder --all-features
    cargo build --release -p baml-agent-runner --all-features

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
# Serves `web/dist` at / when present (`npm run build` in `web/`) so http://127.0.0.1:18080/ loads the chat UI.
runner: build
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    WEB=()
    [ -d web/dist ] && WEB=(--web-dir web/dist)
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio "${WEB[@]}"

# Same as `runner`, but persists provenance to `provenance.db` (SurrealKV on disk).
runner-provenance: build
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    WEB=()
    [ -d web/dist ] && WEB=(--web-dir web/dist)
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio "${WEB[@]}"

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

# Agentium Console: web tests/build + runner routes + publish/deploy parity with Agents view.
verify-agentium-console *args='':
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    chmod +x scripts/verify-agentium-console.sh
    exec ./scripts/verify-agentium-console.sh {{args}}

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
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as clickup-agent, but persists provenance to provenance.db.
clickup-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
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
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as notion-agent, but persists provenance to provenance.db.
notion-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
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
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/slack-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as slack-agent, but persists provenance to provenance.db.
slack-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
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
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as coordinator-agent stack, but adds claude-session + workspace coordinator-agent publish and persists provenance.
coordinator-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
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
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as claude-session-agent, but persists provenance to provenance.db.
claude-session-agent-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Rebuilds coordinator-agent + notion and runs with provenance and UI (HTTP only).
coordinator-notion: web-build build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --web-dir web/dist &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
    {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    wait "$runner_pid"

# Coordinator-agent + Claude session + Notion, HTTP + provenance + web UI.
coordinator-claude-notion: web-build build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_coordinator_claude_notion_db}} --web-dir web/dist &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
    {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
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
    ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
    {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/notion-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/slack-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    wait "$runner_pid"

# Rebuilds coordinator-agent + claude-session + extrospection + clickup + security-eval and runs them with provenance (HTTP only, no stdio).
coordinator-claude-extrospection-clickup: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
    {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir agents/clickup-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    wait "$runner_pid"

# Rebuilds coordinator-agent + claude-session + extrospection + security-eval and runs them via a2a stdio.
coordinator-claude-extrospection: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --a2a-stdio

# Same as coordinator-claude-extrospection, but persists provenance to provenance.db.
coordinator-claude-extrospection-provenance: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(git rev-parse --show-toplevel)"
    set -a
    [ -f .env ] && . ./.env
    set +a
    (
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
      {{builder_bin}} publish --agent-dir agents/coordinator-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/claude-session-demo --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir agents/extrospection-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
      {{builder_bin}} publish --agent-dir tests/fixtures/agents/security-eval-agent --repository-url {{repository_url}} --deploy-url {{runner_base_url}}
    ) &
    exec {{runner_bin}} --serve-http {{runner_http_bind}} --repository-url {{repository_url}} --state-dir {{runner_state_dir}} --repository-dir {{runner_repository_dir}} --provenance-db {{provenance_db}} --a2a-stdio

# Deprecated names: these stacks publish `agents/coordinator-agent`, not the removed persona fixture.
alias persona-notion := coordinator-notion
alias persona-claude-notion := coordinator-claude-notion
alias persona-claude-extrospection-clickup := coordinator-claude-extrospection-clickup
alias persona-claude-extrospection := coordinator-claude-extrospection
alias persona-claude-extrospection-provenance := coordinator-claude-extrospection-provenance

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
      ./scripts/wait-runner-http.sh "{{runner_base_url}}" {{runner_http_ready_secs}}
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

ci_features := "baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-rt-builder/dev-tools,baml-rt-builder/llm-tests,baml-agent-runner/http-tools,baml-agent-runner/memory,baml-rt/llm-tests,baml-agent-runner/llm-tests"

# CI parity: single workspace pass with the union feature set, matching rust-ci.yml.
# Requires: cargo-nextest and OPENROUTER_API_KEY for LLM tests.
# OTEL is disabled for stability/noise reduction in local test runs.
test:
    cargo build -p sandbox-echo-adapter --locked
    OTEL_SDK_DISABLED=true OTEL_TRACES_EXPORTER=none OTEL_METRICS_EXPORTER=none OTEL_LOGS_EXPORTER=none OTEL_EXPORTER_OTLP_ENDPOINT="" cargo nextest run --workspace --locked --profile ci-merged --no-fail-fast --features {{ci_features}}

# Same as `test` but only compile — useful for a quick pre-push check.
test-build:
    cargo nextest run --workspace --features {{ci_features}} --no-run

# Run only a single crate's tests (e.g. `just test-crate baml-rt-provenance`).
test-crate crate:
    cargo nextest run -p {{crate}} --features {{ci_features}}

# Run tests that don't need FalkorDB or API keys (unit tests only).
test-unit:
    cargo nextest run --workspace --features baml-rt-builder/http-tools,baml-rt-builder/dev-tools,baml-agent-runner/http-tools,baml-agent-runner/memory

# Per-file #[test] counts under crates/ (matrix-consolidation triage baseline).
test-inventory:
    #!/usr/bin/env bash
    set -euo pipefail
    bash scripts/test-inventory.sh

# SDK CLI: workspace integrity check
doctor:
    cargo run --release -p cargo-agent-platform -- doctor

# SDK CLI: list all registered tools
list-tools:
    cargo run --release -p cargo-agent-platform -- list-tools

# SDK CLI: list all agent packages
list-agents:
    cargo run --release -p cargo-agent-platform -- list-agents

# Run E2E k8s tests against a real k3d cluster (Argo CD + local registry).
e2e-k8s:
    ./scripts/e2e-k8s/run.sh

# Local k3d dev stack: cluster + registry + build/push + Argo sync.
up:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{justfile_directory()}}"
    # shellcheck source=scripts/e2e-k8s/lib.sh
    source "${root}/scripts/e2e-k8s/lib.sh"
    REPO_ROOT="${root}"
    preflight_container_runtime
    ensure_k3d_cluster
    ensure_image_tag_or_nonce
    docker build --build-arg "VERSION=$(bash "${root}/scripts/release/workspace-version.sh")" \
      -t "${IMAGE_NAME}:${IMAGE_TAG}" "${root}"
    bringup_pilot_stack
    log_info "Pilot stack ready (tag=${IMAGE_TAG})"

# Rebuild runner image and re-sync Argo after code changes.
sync:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{justfile_directory()}}"
    # shellcheck source=scripts/e2e-k8s/lib.sh
    source "${root}/scripts/e2e-k8s/lib.sh"
    REPO_ROOT="${root}"
    bash "${root}/scripts/e2e-k8s/image-tag-nonce.sh"
    resolve_image_tag
    docker build --build-arg "VERSION=$(bash "${root}/scripts/release/workspace-version.sh")" \
      -t "${IMAGE_NAME}:${IMAGE_TAG}" "${root}"
    ensure_k3d_registry
    connect_registry_to_cluster
    ensure_runner_image_available
    bash "${root}/scripts/e2e-k8s/render-values.sh"
    bash "${root}/scripts/e2e-k8s/sync-argocd.sh"
    resolve_chart_names
    wait_for_runner_readyz

workspace-version:
    bash scripts/release/workspace-version.sh

bump-version kind:
    bash scripts/release/bump-workspace-version.sh {{kind}}

image-tag-nonce:
    bash scripts/e2e-k8s/image-tag-nonce.sh

render-values:
    bash scripts/e2e-k8s/render-values.sh

sync-argocd:
    bash scripts/e2e-k8s/sync-argocd.sh

release *ARGS:
    bash scripts/release/set-deploy-ref.sh {{ARGS}}

publish-release *ARGS:
    bash scripts/release/publish-deploy-release.sh {{ARGS}}

# Run the Kubernetes pilot first-run smoke against an installed release.
# Prerequisites: a running chart install (see docs/k8s-pilot-operator-guide.md)
# and an open port-forward, or pass `--port-forward` to have the script manage it.
k8s-pilot-smoke *args='':
    ./scripts/k8s-pilot-smoke.sh {{args}}

# Adversarial cgroup-throttled deploy harness on a real k3d cluster.
# Probes /readyz + /diagnose at ~100ms during a cpu-peg-agent deploy under
# runner.resources.limits.cpu=500m and asserts three runner-readiness
# invariants. See docs/testing/e2e-k8s.md.
e2e-k8s-cgroup-throttle *args='':
    ./scripts/e2e-k8s/t2-cgroup-throttle.sh {{args}}

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

# Cross-pod cleese/chapman conversation on a Helm-installed pilot.
# Requires OPENROUTER_API_KEY in the mounted fnox ConfigMap.
k8s-pilot-cleese-chapman *args='':
    ./scripts/k8s-pilot-cleese-chapman.sh {{args}}

# Pilot demo prep flow.
# Steps chain via --keep-cluster / --keep-deployed so the cluster
# persists across all three. cleese/chapman needs OPENROUTER_API_KEY
# in the mounted fnox ConfigMap; use `demo-rehearsal-no-llm` to stop
# after smoke when LLM credentials are not configured.
demo-rehearsal: demo-rehearsal-no-llm
    just k8s-pilot-cleese-chapman --keep-deployed
    just demo-rehearsal-assert

# verify + smoke only; no LLM required.
demo-rehearsal-no-llm:
    just verify-k8s-pilot-package --keep-cluster --smoke-keep-deployed
    just demo-rehearsal-assert

# Customer-facing cluster-health checks (issue #391):
#   - placement consistency (I1/I2/I3) + heartbeat freshness (I4) via
#     /cluster/agents on the runner API service.
#   - no-unexpected-WARN log scan (I5) across runner + SurrealDB pods.
# Both fail the rehearsal on regression.
demo-rehearsal-assert:
    ./scripts/k8s-pilot-assert-placement-consistency.sh --port-forward --local-port 18181
    ./scripts/k8s-pilot-assert-no-warn-logs.sh
