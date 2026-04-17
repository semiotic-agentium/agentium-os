#!/usr/bin/env python3
"""Concurrent message.sendStream SSE load: N independent sessions (distinct contextId per stream).

Use this to stress Surreal/provenance/QuickJS under parallel streams. Same context would hit
Conflict in the live-stream path; this script gives each stream its own context_id.

Example (runner on 18080, stream-js-tool fixture):
  python3 scripts/concurrent_a2a_sse.py --package stream-js-tool --text 'stream-task load' --concurrency 8

Requires a running baml-agent-runner with the agent deployed (see scripts/verify-runner-http.sh).
"""
from __future__ import annotations

import argparse
import http.client
import json
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed


def run_one_stream(
    host: str,
    port: int,
    package: str,
    instance: str,
    text: str,
    base_ms: int,
    idx: int,
    timeout_sec: float,
) -> dict:
    """POST one SSE stream; return timing fields and status (or error)."""
    path = f"/agents/{package}/{instance}/a2a/sse"
    context_id = f"ctx-{base_ms}-{idx}"
    message_id = f"m-conc-{idx}-{base_ms}"
    corr_id = f"corr-{base_ms}-{idx}"
    body = {
        "jsonrpc": "2.0",
        "id": corr_id,
        "method": "message.sendStream",
        "params": {
            "message": {
                "messageId": message_id,
                "role": "user",
                "contextId": context_id,
                "parts": [{"text": text}],
            }
        },
    }
    raw = json.dumps(body).encode("utf-8")
    t_request = time.perf_counter()
    err: str | None = None
    status = -1
    first_body_byte: float | None = None
    first_sse_data: float | None = None
    t_stream_end: float | None = None
    t_after_headers: float | None = None
    try:
        conn = http.client.HTTPConnection(host, port, timeout=timeout_sec)
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
            err = resp.read(2000).decode("utf-8", errors="replace")
            t_stream_end = time.perf_counter()
            return {
                "idx": idx,
                "context_id": context_id,
                "http_status": status,
                "error": err,
                "request_ms": (t_after_headers - t_request) * 1000,
            }

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
                if stripped.startswith(b"data:") and first_sse_data is None:
                    first_sse_data = time.perf_counter()
        t_stream_end = time.perf_counter()
    except Exception as e:
        err = str(e)
        t_stream_end = time.perf_counter()

    def ms(a: float | None, b: float) -> float | None:
        if a is None:
            return None
        return (a - b) * 1000

    req = t_request
    return {
        "idx": idx,
        "context_id": context_id,
        "http_status": status,
        "error": err,
        "time_to_response_headers_ms": ms(t_after_headers, req) if t_after_headers else None,
        "time_to_first_body_byte_ms": ms(first_body_byte, req),
        "time_to_first_sse_data_line_ms": ms(first_sse_data, req),
        "time_to_stream_end_ms": ms(t_stream_end, req) if t_stream_end else None,
    }


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=18080)
    p.add_argument("--package", required=True)
    p.add_argument("--instance", default="default")
    p.add_argument(
        "--text",
        default="stream-task load",
        help="User message (stream-js-tool needs 'stream-task' substring to stream)",
    )
    p.add_argument("--concurrency", type=int, default=4, help="Parallel SSE connections")
    p.add_argument(
        "--timeout",
        type=float,
        default=300.0,
        help="Per-connection socket timeout (seconds)",
    )
    args = p.parse_args()
    if args.concurrency < 1 or args.concurrency > 10_000:
        print("concurrency out of range", file=sys.stderr)
        return 2

    base_ms = int(time.time() * 1000)
    wall_start = time.perf_counter()
    results: list[dict] = []

    with ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        futs = [
            ex.submit(
                run_one_stream,
                args.host,
                args.port,
                args.package,
                args.instance,
                args.text,
                base_ms,
                idx,
                args.timeout,
            )
            for idx in range(args.concurrency)
        ]
        for fut in as_completed(futs):
            results.append(fut.result())

    wall_elapsed = time.perf_counter() - wall_start
    results.sort(key=lambda r: r["idx"])

    print(f"concurrent_a2a_sse: concurrency={args.concurrency} package={args.package} wall_s={wall_elapsed:.3f}")
    print(f"base_ms={base_ms} (contextId ctx-{{base}}-{{idx}}, jsonrpc id corr-{{base}}-{{idx}})")
    ok = [r for r in results if r.get("http_status") == 200 and r.get("error") is None]
    bad = [r for r in results if r not in ok]

    for r in results:
        line = (
            f"  idx={r['idx']:4d} context={r['context_id']} status={r.get('http_status')} "
            f"hdr_ms={r.get('time_to_response_headers_ms')} "
            f"first_sse_ms={r.get('time_to_first_sse_data_line_ms')} "
            f"end_ms={r.get('time_to_stream_end_ms')}"
        )
        if r.get("error"):
            line += f" err={r['error'][:200]!r}"
        print(line)

    ends = [r["time_to_stream_end_ms"] for r in ok if r.get("time_to_stream_end_ms") is not None]
    firsts = [r["time_to_first_sse_data_line_ms"] for r in ok if r.get("time_to_first_sse_data_line_ms") is not None]
    hdrs = [r["time_to_response_headers_ms"] for r in ok if r.get("time_to_response_headers_ms") is not None]

    def stat_line(name: str, xs: list[float]) -> None:
        if not xs:
            return
        xs_sorted = sorted(xs)
        p50 = xs_sorted[len(xs_sorted) // 2]
        p95 = xs_sorted[int(len(xs_sorted) * 0.95) - 1] if len(xs_sorted) >= 2 else xs_sorted[-1]
        print(
            f"summary_ok_streams={len(ok)}/{args.concurrency} {name}_ms "
            f"min={min(xs):.2f} p50={p50:.2f} max={max(xs):.2f} p95={p95:.2f} mean={statistics.mean(xs):.2f}"
        )

    stat_line("time_to_response_headers", hdrs)
    stat_line("time_to_first_sse_data_line", firsts)
    stat_line("time_to_stream_end", ends)

    if bad:
        print(f"failures={len(bad)} (see per-line err)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
