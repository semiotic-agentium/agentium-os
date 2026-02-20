#!/usr/bin/env bash
set -euo pipefail

PID_FILE="${COORDINATOR_DEMO_PID:-/tmp/coordinator-runner.pid}"

if [ -f "$PID_FILE" ]; then
  PID="$(cat "$PID_FILE" || true)"
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" || true
    for _ in $(seq 1 20); do
      if ! kill -0 "$PID" 2>/dev/null; then
        break
      fi
      sleep 0.25
    done
    if kill -0 "$PID" 2>/dev/null; then
      kill -9 "$PID" || true
    fi
    echo "Stopped runner (pid $PID)"
  fi
  rm -f "$PID_FILE"
fi
