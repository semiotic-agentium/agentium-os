#!/usr/bin/env bash
set -euo pipefail

ACTION="${1:-}"
ROOT="$(git rev-parse --show-toplevel)"
STACK_DIR="$ROOT/observability"

if [[ -z "$ACTION" ]]; then
  echo "Usage: $0 <up|down|ps|logs>" >&2
  exit 1
fi

cd "$STACK_DIR"

case "$ACTION" in
  up)
    docker compose up -d
    echo "Grafana:    http://localhost:3000 (admin/admin)"
    echo "Prometheus: http://localhost:9090"
    ;;
  down)
    docker compose down
    ;;
  ps)
    docker compose ps
    ;;
  logs)
    docker compose logs -f --tail=200
    ;;
  *)
    echo "Unknown action: $ACTION" >&2
    echo "Usage: $0 <up|down|ps|logs>" >&2
    exit 1
    ;;
esac
