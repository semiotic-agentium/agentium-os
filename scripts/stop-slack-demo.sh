#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

PID_FILE="${SLACK_DEMO_PID:-/tmp/slack-runner.pid}"

if [ -f "$PID_FILE" ]; then
  PID="$(cat "$PID_FILE" || true)"
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" || true
    echo "Stopped runner (pid $PID)"
  fi
  rm -f "$PID_FILE"
fi
