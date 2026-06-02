#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Stop any process listening on host:port (e.g. 127.0.0.1:18080).
# Used before starting a fresh release runner so publish does not hit a stale binary.
set -euo pipefail

bind="${1:?bind address required (e.g. 127.0.0.1:18080)}"
host="${bind%%:*}"
port="${bind##*:}"
if [ -z "$port" ] || [ "$port" = "$bind" ]; then
  echo "error: expected host:port, got: $bind" >&2
  exit 1
fi

LISTEN_PID="$(lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null | head -n 1 || true)"
if [ -z "$LISTEN_PID" ]; then
  exit 0
fi

cmd="$(ps -p "$LISTEN_PID" -o command= 2>/dev/null || true)"
echo "Port ${port} already in use by pid ${LISTEN_PID}${cmd:+ ($cmd)}; stopping it..." >&2
kill "$LISTEN_PID" || true
for _ in $(seq 1 20); do
  if ! kill -0 "$LISTEN_PID" 2>/dev/null; then
    exit 0
  fi
  sleep 0.25
done
if kill -0 "$LISTEN_PID" 2>/dev/null; then
  kill -9 "$LISTEN_PID" || true
fi
if kill -0 "$LISTEN_PID" 2>/dev/null; then
  echo "error: failed to free port ${port} (pid ${LISTEN_PID})" >&2
  exit 1
fi
