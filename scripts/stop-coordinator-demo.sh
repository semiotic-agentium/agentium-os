#!/usr/bin/env bash
set -euo pipefail

PID_FILE="${COORDINATOR_DEMO_PID:-/tmp/coordinator-runner.pid}"
PORT="${COORDINATOR_DEMO_PORT:-8082}"

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
    if kill -0 "$PID" 2>/dev/null; then
      echo "Failed to stop runner from pid file (pid $PID)" >&2
      exit 1
    fi
    echo "Stopped runner (pid $PID)"
  fi
  rm -f "$PID_FILE"
fi

LISTEN_PID="$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | head -n 1 || true)"
if [ -n "$LISTEN_PID" ]; then
  kill "$LISTEN_PID" || true
  for _ in $(seq 1 20); do
    if ! kill -0 "$LISTEN_PID" 2>/dev/null; then
      break
    fi
    sleep 0.25
  done
  if kill -0 "$LISTEN_PID" 2>/dev/null; then
    kill -9 "$LISTEN_PID" || true
  fi
  if kill -0 "$LISTEN_PID" 2>/dev/null; then
    echo "Failed to stop listener on port $PORT (pid $LISTEN_PID)" >&2
    exit 1
  fi
  echo "Stopped listener on port $PORT (pid $LISTEN_PID)"
fi
