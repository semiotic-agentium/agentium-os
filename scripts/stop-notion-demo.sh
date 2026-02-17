#!/usr/bin/env bash
set -euo pipefail

PID_FILE="${NOTION_DEMO_PID:-/tmp/notion-runner.pid}"
FALKORDB_FLAG="${NOTION_DEMO_FALKORDB_FLAG:-/tmp/notion-falkordb.started}"

if [ -f "$PID_FILE" ]; then
  PID="$(cat "$PID_FILE" || true)"
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" || true
    echo "Stopped runner (pid $PID)"
  fi
  rm -f "$PID_FILE"
fi

if [ -f "$FALKORDB_FLAG" ]; then
  if [ -x ./scripts/falkordb.sh ]; then
    ./scripts/falkordb.sh down >/dev/null 2>&1 || true
    echo "Stopped FalkorDB"
  fi
  rm -f "$FALKORDB_FLAG"
fi
