#!/usr/bin/env bash
# Wait until the local runner responds on routes needed before `publish` / push.
# Usage: wait-runner-http.sh <base_url> [timeout_secs]
set -euo pipefail

base="${1:?runner base URL required (e.g. http://127.0.0.1:18080)}"
secs="${2:-180}"

deadline=$(( $(date +%s) + secs ))
while [ "$(date +%s)" -lt "$deadline" ]; do
  if curl -sf "$base/openapi.json" >/dev/null 2>&1 \
    && curl -sf "$base/repository/agents" >/dev/null 2>&1; then
    exit 0
  fi
  sleep 0.5
done

echo "error: runner HTTP not ready at $base within ${secs}s (need GET /openapi.json and GET /repository/agents). Is another process using the bind address? Check runner logs." >&2
exit 1
