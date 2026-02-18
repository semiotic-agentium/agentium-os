#!/usr/bin/env bash
set -euo pipefail

PID_FILE="${NOTION_DEMO_PID:-/tmp/notion-runner.pid}"

if [ -f "$PID_FILE" ]; then
  PID="$(cat "$PID_FILE" || true)"
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" || true
    echo "Stopped runner (pid $PID)"
  fi
  rm -f "$PID_FILE"
fi
