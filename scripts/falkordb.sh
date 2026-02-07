#!/usr/bin/env bash
set -euo pipefail

NAME="falkordb"
IMAGE="falkordb/falkordb:latest"
PORTS=(-p 6379:6379 -p 8080:8080 -p 3000:3000)

cmd="${1:-}"

case "$cmd" in
  up)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker run -d --name "$NAME" "${PORTS[@]}" "$IMAGE"
    ;;
  down)
    docker rm -f "$NAME"
    ;;
  restart)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker run -d --name "$NAME" "${PORTS[@]}" "$IMAGE"
    ;;
  status)
    docker ps --filter "name=^/${NAME}$"
    ;;
  logs)
    docker logs -f "$NAME"
    ;;
  *)
    echo "Usage: $0 {up|down|restart|status|logs}" >&2
    exit 1
    ;;
esac
