#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

"""Inspect tool adapter contract.

For `describe` and `schema`, this script reads the bind runtime artifact:
  <rootfs>/etc/agentium/tool-bundle.json

For `invoke`, it sends framed TSRPC JSON-RPC to an adapter command you provide
after `--`.

Examples:
  ./inspect_tool.py describe
  ./inspect_tool.py schema --rootfs ./.tmp/dev-meteo-tool-rootfs
  ./inspect_tool.py invoke --input '{"location_query":"Athens, Greece"}' -- docker run --rm -i dev-meteo-tool-sandbox:local /tool-adapter
"""

import argparse
import json
import pathlib
import struct
import subprocess

TOOL_ID = "dev/meteo-tool"
DEFAULT_ROOTFS = pathlib.Path(__file__).resolve().parent / ".tmp" / "dev-meteo-tool-rootfs"


def _read_bundle(rootfs: pathlib.Path) -> dict:
    bundle_path = rootfs / "etc" / "agentium" / "tool-bundle.json"
    if not bundle_path.exists():
        raise SystemExit(
            f"missing sidecar bundle at {bundle_path}\n"
            "Hint: run ./setup_bind_sandbox.sh --force or sandbox-bind-sync first."
        )
    try:
        return json.loads(bundle_path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"failed to parse {bundle_path}: {exc}")


def _describe_from_bundle(bundle: dict) -> dict:
    manifest = bundle.get("manifest") or {}
    schema = bundle.get("schema") or {}
    runtime = bundle.get("runtime") or {}
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocol_version": manifest.get("protocol_version", "2"),
            "tool_name": manifest.get("tool_name", runtime.get("tool_id", TOOL_ID)),
            "supported_methods": manifest.get(
                "supported_methods", ["tool/describe", "tool/schema", "tool/invoke"]
            ),
            "schema_digest": schema.get("content_digest"),
        },
    }


def _schema_from_bundle(bundle: dict) -> dict:
    schema = bundle.get("schema")
    if not isinstance(schema, dict):
        raise SystemExit("sidecar bundle missing schema object")
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "result": schema,
    }


def _build_invoke_request(input_json: str) -> dict:
    try:
        input_obj = json.loads(input_json)
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"invalid --input JSON: {exc}")

    return {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tool/invoke",
        "params": {
            "invocation_id": "inspect",
            "tool_name": TOOL_ID,
            "input": input_obj,
        },
    }


def _invoke_via_adapter(req: dict, cmd: list[str]) -> dict:
    body = json.dumps(req, separators=(",", ":")).encode("utf-8")
    frame = struct.pack(">I", len(body)) + body

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    out, err = proc.communicate(frame, timeout=15)

    if proc.returncode != 0:
        stderr_text = err.decode("utf-8", errors="replace").strip()
        raise SystemExit(
            f"adapter command failed (exit={proc.returncode}): {stderr_text or '(no stderr)'}"
        )

    if len(out) < 4:
        raise SystemExit(f"short framed output: {len(out)} bytes")

    (size,) = struct.unpack(">I", out[:4])
    payload = out[4 : 4 + size]
    if len(payload) != size:
        raise SystemExit(f"truncated payload: expected {size} bytes, got {len(payload)}")

    try:
        return json.loads(payload.decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"response payload is not valid JSON: {exc}")


def _parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["describe", "schema", "invoke"])
    parser.add_argument(
        "--rootfs",
        default=str(DEFAULT_ROOTFS),
        help="bind rootfs path for describe/schema artifact inspection",
    )
    parser.add_argument(
        "--input",
        default='{"location_query":"Athens, Greece"}',
        help="invoke input JSON (used only for mode=invoke)",
    )
    parser.add_argument(
        "cmd",
        nargs=argparse.REMAINDER,
        help="adapter command for mode=invoke (pass after --)",
    )
    args = parser.parse_args()

    cmd = list(args.cmd or [])
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]

    return args, cmd


def main() -> int:
    args, cmd = _parse_args()

    if args.mode in ("describe", "schema"):
        bundle = _read_bundle(pathlib.Path(args.rootfs))
        resp = _describe_from_bundle(bundle) if args.mode == "describe" else _schema_from_bundle(bundle)
        print(json.dumps(resp, indent=2, ensure_ascii=False))
        return 0

    if not cmd:
        raise SystemExit("mode=invoke requires adapter command after '--' (e.g. docker run ... /tool-adapter)")

    req = _build_invoke_request(args.input)
    resp = _invoke_via_adapter(req, cmd)
    print(json.dumps(resp, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
