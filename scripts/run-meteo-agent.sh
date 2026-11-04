#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_URL="${RUNNER_URL:-http://127.0.0.1:18080}"
RUNNER_BIND="${RUNNER_BIND:-127.0.0.1:18080}"
REPOSITORY_URL="${REPOSITORY_URL:-${RUNNER_URL}/repository}"
RUNTIME_IMAGE_REF="localhost:5001/karakara/tool-runtime:node-22"
KARAK_BIN="${CARGO_TARGET_DIR:-target}/release/karak"
RUNNER_LOG="${RUNNER_LOG:-/tmp/meteo-agent-runner.log}"
RUNNER_PID=""

cd "$ROOT_DIR"

log() {
  printf '\n==> %s\n' "$*"
}

runner_curl() {
  local args=(-fsS)
  if [[ -n "${RUNNER_TOKEN:-}" ]]; then
    args+=(-H "X-Runner-Token: ${RUNNER_TOKEN}")
  fi
  curl "${args[@]}" "$@"
}

cleanup() {
  if [[ -n "$RUNNER_PID" ]] && kill -0 "$RUNNER_PID" 2>/dev/null; then
    kill "$RUNNER_PID" 2>/dev/null || true
    wait "$RUNNER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

log "Checking the runner"
if runner_curl "$RUNNER_URL/readyz" >/dev/null 2>&1; then
  echo "Runner is already running at $RUNNER_URL"
else
  if [[ -z "${KARAKARA_KVM_IMAGE:-}" ]]; then
    # Rebuild the runtime image so the in-VM supervisor reflects the current
    # (incl. uncommitted) source. The Dockerfile builds baml-tool-supervisor
    # from `.` inside a Rust stage, so a plain `docker push` of a stale image
    # would keep running an old supervisor in the VM. --provenance=false: the
    # microsandbox image loader expects the platform manifest directly.
    # Set REBUILD_RUNTIME_IMAGE=0 to skip (reuse the last-built image).
    if [[ "${REBUILD_RUNTIME_IMAGE:-1}" == "1" ]]; then
      log "Rebuilding the local runtime image (includes uncommitted supervisor changes)"
      docker build --provenance=false \
        -f docker/tool-runtime/node.Dockerfile \
        -t "$RUNTIME_IMAGE_REF" .
    fi
    log "Publishing the local runtime image"
    docker push "$RUNTIME_IMAGE_REF" >/dev/null
    runtime_digest="$(docker image inspect "$RUNTIME_IMAGE_REF" \
      --format '{{range .RepoDigests}}{{println .}}{{end}}' \
      | awk -F@ '$1 == "localhost:5001/karakara/tool-runtime" { print $2; exit }')"
    KARAKARA_KVM_IMAGE="localhost:5001/karakara/tool-runtime@$runtime_digest"
  fi

  if [[ -z "$KARAKARA_KVM_IMAGE" || "$KARAKARA_KVM_IMAGE" != *@sha256:* ]]; then
    cat >&2 <<'EOF'
KARAKARA_KVM_IMAGE is not set to a digest-pinned image.
Build and push the runtime image first, then export for example:

  export KARAKARA_KVM_IMAGE="$(docker image inspect \
    localhost:5001/karakara/tool-runtime:node-22 \
    --format '{{index .RepoDigests 0}}')"
EOF
    exit 1
  fi

  log "Starting the runner with $KARAKARA_KVM_IMAGE"
  # Start HTTP-only. Do not use `just runner-provenance` here because it also
  # starts the A2A stdio loop, which exits on /dev/null and stops the server.
  log "Building the runner"
  just build >>"$RUNNER_LOG" 2>&1
  KARAKARA_KVM_IMAGE="$KARAKARA_KVM_IMAGE" "$KARAK_BIN" serve \
    --serve-http "$RUNNER_BIND" \
    --repository-url "$REPOSITORY_URL" \
    --state-dir .runner-state \
    --repository-dir .repository \
    --provenance-db provenance.db \
    >"$RUNNER_LOG" 2>&1 < /dev/null &
  RUNNER_PID=$!

  for _ in {1..90}; do
    if runner_curl "$RUNNER_URL/readyz" >/dev/null 2>&1; then
      break
    fi
    if ! kill -0 "$RUNNER_PID" 2>/dev/null; then
      cat "$RUNNER_LOG" >&2
      exit 1
    fi
    sleep 1
  done

  runner_curl "$RUNNER_URL/readyz" >/dev/null
  echo "Runner is ready"
fi

log "Removing older meteo-agent deployments"
deployments="$(runner_curl "$RUNNER_URL/deployments")"
mapfile -t old_hashes < <(
  printf '%s' "$deployments" | python3 -c '
import json, sys
for row in json.load(sys.stdin):
    if row.get("agent_name") == "meteo-agent":
        print(row["content_hash"])
'
)

for hash in "${old_hashes[@]}"; do
  [[ -z "$hash" ]] && continue
  echo "Undeploying $hash"
  runner_curl -X POST \
    -H 'content-type: application/json' \
    --data "$(python3 - "$hash" <<'PY'
import json
import sys
print(json.dumps({
    "hash": sys.argv[1],
    "override_rationale": "Local Meteo v2 validation: replace the stale local deployment",
}))
PY
)" \
    "$RUNNER_URL/undeploy"
  echo
 done

log "Building and testing dev/meteo-tool"
cd "$ROOT_DIR/examples/external-tools/meteo-tool"
npm install
cd "$ROOT_DIR"
cargo run -q -p karakara-os -- tool build examples/external-tools/meteo-tool
cargo run -q -p karakara-os -- tool test \
  examples/external-tools/meteo-tool \
  --input '{"location_query":"Athens, Greece","hourly_limit":2}'

log "Publishing dev/meteo-tool"
cargo run -q -p karakara-os -- tool publish \
  examples/external-tools/meteo-tool \
  --repository-url "$REPOSITORY_URL" \
  --approved-by local-dev \
  --yes

log "Publishing and deploying meteo-agent"
cargo run -q -p karakara-os -- install agent \
  --path examples/agents/meteo-agent \
  --repository-url "$REPOSITORY_URL" \
  --url "$RUNNER_URL" \
  --rationale "meteo v2 validation"

log "Starting chat"
cargo run -q -p karakara-os -- chat \
  --agent meteo-agent \
  --url "$RUNNER_URL"
