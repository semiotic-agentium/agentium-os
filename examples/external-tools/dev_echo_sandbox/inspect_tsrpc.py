#!/usr/bin/env python3
"""
Quick TSRPC inspector for sandbox adapter binaries.

Examples:
  # describe
  ./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py \
    --adapter ./.tmp/dev-echo-rootfs/tool-adapter describe

  # invoke
  ./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py \
    --adapter ./.tmp/dev-echo-rootfs/tool-adapter invoke \
    --message "hello-echo?"
"""

from __future__ import annotations

import argparse
import json
import struct
import subprocess
import sys
from pathlib import Path


def send_frame(proc: subprocess.Popen[bytes], payload: dict) -> None:
    body = json.dumps(payload).encode("utf-8")
    proc.stdin.write(struct.pack(">I", len(body)))
    proc.stdin.write(body)
    proc.stdin.flush()


def recv_frame(proc: subprocess.Popen[bytes]) -> dict:
    hdr = proc.stdout.read(4)
    if len(hdr) < 4:
        raise RuntimeError("no frame header received from adapter")
    (size,) = struct.unpack(">I", hdr)
    data = proc.stdout.read(size)
    if len(data) < size:
        raise RuntimeError(f"short frame body: got {len(data)} expected {size}")
    return json.loads(data.decode("utf-8"))


def run(adapter: Path, request: dict, timeout_s: float) -> int:
    if not adapter.exists():
        print(f"adapter not found: {adapter}", file=sys.stderr)
        return 2

    proc = subprocess.Popen(
        [str(adapter)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        assert proc.stdin and proc.stdout and proc.stderr
        send_frame(proc, request)
        response = recv_frame(proc)
        print(json.dumps(response, indent=2))
        return 0
    except Exception as exc:
        print(f"request failed: {exc}", file=sys.stderr)
        try:
            err = proc.stderr.read().decode("utf-8", errors="replace")
            if err:
                print("--- adapter stderr ---", file=sys.stderr)
                print(err, file=sys.stderr)
        except Exception:
            pass
        return 1
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=timeout_s)
        except Exception:
            proc.kill()


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser()
    p.add_argument(
        "--adapter",
        default="./.tmp/dev-echo-rootfs/tool-adapter",
        help="path to adapter binary",
    )
    p.add_argument("--timeout", type=float, default=3.0, help="process shutdown timeout")

    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("describe")

    inv = sub.add_parser("invoke")
    inv.add_argument("--message", default="hello-echo?", help="input.message")
    inv.add_argument("--tool-name", default="dev/echo", help="params.tool_name")
    inv.add_argument("--invocation-id", default="manual-1", help="params.invocation_id")

    return p


def main() -> int:
    args = build_parser().parse_args()
    adapter = Path(args.adapter)

    if args.cmd == "describe":
        req = {"jsonrpc": "2.0", "id": 1, "method": "tool/describe", "params": {}}
    else:
        req = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tool/invoke",
            "params": {
                "invocation_id": args.invocation_id,
                "tool_name": args.tool_name,
                "input": {"message": args.message},
                "secrets": {},
                "capabilities": None,
            },
        }

    return run(adapter, req, args.timeout)


if __name__ == "__main__":
    raise SystemExit(main())
