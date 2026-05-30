#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

"""POST message.sendStream to A2A SSE and print timing (TTFB, first data line, stream end).

For N parallel streams with distinct contextId (multi-session load), use scripts/concurrent_a2a_sse.py.
"""
from __future__ import annotations

import argparse
import http.client
import json
import sys
import time


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=18080)
    p.add_argument("--package", required=True)
    p.add_argument("--instance", default="default")
    p.add_argument("--text", required=True, help="User message text")
    args = p.parse_args()

    path = f"/agents/{args.package}/{args.instance}/a2a/sse"
    body = {
        "jsonrpc": "2.0",
        "id": "placeholder-replaced-below",
        "method": "message.sendStream",
        "params": {
            "message": {
                "messageId": "m-measure",
                "role": "user",
                "parts": [{"text": args.text}],
            }
        },
    }
    corr_id = f"corr-{int(time.time() * 1000)}-1"
    body["id"] = corr_id

    raw = json.dumps(body).encode("utf-8")

    conn = http.client.HTTPConnection(args.host, args.port, timeout=300)
    t_request = time.perf_counter()
    conn.request(
        "POST",
        path,
        raw,
        headers={"Content-Type": "application/json", "Accept": "text/event-stream"},
    )
    resp = conn.getresponse()
    t_after_headers = time.perf_counter()

    status = resp.status
    if status != 200:
        err = resp.read()
        print(f"HTTP {status}: {err[:800]!r}", file=sys.stderr)
        return 1

    first_body_byte: float | None = None
    first_sse_data_line: float | None = None
    carry = b""

    while True:
        chunk = resp.read(4096)
        if not chunk:
            break
        if first_body_byte is None:
            first_body_byte = time.perf_counter()
        carry += chunk
        while b"\n" in carry:
            line, carry = carry.split(b"\n", 1)
            stripped = line.strip()
            if stripped.startswith(b"data:") and first_sse_data_line is None:
                first_sse_data_line = time.perf_counter()

    t_stream_end = time.perf_counter()

    def ms(t: float | None, base: float) -> str:
        if t is None:
            return "n/a"
        return f"{(t - base) * 1000:.2f}"

    print(f"path={path}")
    print(f"http_status={status}")
    print(f"time_to_response_headers_ms={ms(t_after_headers, t_request)}")
    print(f"time_to_first_response_body_byte_ms={ms(first_body_byte, t_request)}")
    print(f"time_to_first_sse_data_line_ms={ms(first_sse_data_line, t_request)}")
    print(f"time_to_stream_end_ms={ms(t_stream_end, t_request)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
