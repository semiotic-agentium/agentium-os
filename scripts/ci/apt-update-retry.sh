#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Refresh APT indices with retries. Mitigates transient failures when
# archive.ubuntu.com is mid-sync ("File has unexpected size" / hash mismatch).
set -euo pipefail

sudo install -m 644 /dev/stdin /etc/apt/apt.conf.d/80-ci-retries <<'EOF'
Acquire::Retries "5";
Acquire::http::Timeout "120";
EOF

readonly max_attempts="${APT_UPDATE_MAX_ATTEMPTS:-5}"
readonly sleep_s="${APT_UPDATE_RETRY_SLEEP_S:-30}"

for attempt in $(seq 1 "$max_attempts"); do
  if sudo apt-get update; then
    exit 0
  fi
  echo "apt-get update failed (attempt ${attempt}/${max_attempts}); sleeping ${sleep_s}s before retry..." >&2
  if [ "$attempt" -lt "$max_attempts" ]; then
    sleep "$sleep_s"
  fi
done

exit 1
