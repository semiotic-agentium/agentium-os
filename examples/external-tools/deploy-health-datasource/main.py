#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

"""Raw external datasource example (JSON-RPC over stdio).

A *raw* datasource is parsed in-process by the runner: the webhook body becomes
`messages[0]` directly. The tool process is therefore only ever spawned for
discovery (`tool/describe` + `tool/schema`); it is never invoked per webhook.
So this script only has to advertise the event contract via `events[]`.

The `content_digest` returned from `tool/schema` MUST equal the runner's
`schema_digest_from_events`, which is a SHA-256 over the JCS-canonical
(RFC 8785: sorted keys, no whitespace) encoding of `{"events": events}`.
`json.dumps(..., sort_keys=True, separators=(",", ":"))` reproduces that for
ASCII object/string/array/bool content.
"""

import hashlib
import json
import os
import struct
import sys
import traceback
from datetime import datetime, timezone

PROTOCOL_VERSION = "1"
METHOD_DESCRIBE = "tool/describe"
METHOD_SCHEMA = "tool/schema"
METHOD_INVOKE = "tool/invoke"
SUPPORTED_METHODS = [METHOD_DESCRIBE, METHOD_SCHEMA]
DEFAULT_SCHEMA_CONTENT_TYPE = "application/schema+json"

ERR_METHOD_NOT_FOUND = -32601
ERR_PARSE_ERROR = -32700
ERR_INVALID_PARAMS = -32602
ERR_INTERNAL = -32000

TOOL_NAME = "examples/deploy-health-datasource"
SCHEMA_VERSION = "deploy-health.v1"
DEBUG = os.environ.get("DEPLOY_HEALTH_DEBUG", "0") not in ("0", "false", "False")

# One event kind. Lenient (`additionalProperties: true`) so additive changes to
# the provider's payload surface as runtime validation tolerance, not drift.
EVENTS = [
    {
        "schema_version": SCHEMA_VERSION,
        "name": "DeployHealthEvent",
        "schema": {
            "type": "object",
            "additionalProperties": True,
            "properties": {
                "service": {"type": "string"},
                "environment": {"type": "string"},
                "status": {"type": "string"},
                "deploy_id": {"type": "string"},
                "observed_at": {"type": "string"},
            },
            "required": ["service", "status"],
        },
    }
]


def _events_digest():
    canonical = json.dumps(
        {"events": EVENTS},
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return "sha256:" + hashlib.sha256(canonical).hexdigest()


def _schema_result():
    # Datasource tools omit input/output and return the event contract instead.
    return {
        "schema_version": 1,
        "tool_name": TOOL_NAME,
        "content_type": DEFAULT_SCHEMA_CONTENT_TYPE,
        "content_digest": _events_digest(),
        "events": EVENTS,
    }


def _log(msg):
    if not DEBUG:
        return
    now = datetime.now(timezone.utc).isoformat()
    print(f"[{now}] {TOOL_NAME} {msg}", file=sys.stderr, flush=True)


def _encode_response(req_id=None, result=None, error=None):
    if error is None:
        return {"jsonrpc": "2.0", "id": req_id, "result": result}
    return {"jsonrpc": "2.0", "id": req_id, "error": error}


def _write_raw(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def _write_framed(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(body)))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def write_result(req_id, result, framed=False):
    payload = _encode_response(req_id=req_id, result=result)
    _write_framed(payload) if framed else _write_raw(payload)


def write_error(req_id, code, message, error_class="execution", framed=False):
    payload = _encode_response(
        req_id=req_id,
        error={"code": code, "message": message, "data": {"error_class": error_class}},
    )
    _write_framed(payload) if framed else _write_raw(payload)


def _read_request():
    first4 = sys.stdin.buffer.read(4)
    if not first4:
        return None, False

    # TSRPC framed mode: 4-byte BE length + JSON body.
    if any((b < 32 and b not in (9, 10, 13)) for b in first4):
        size = struct.unpack(">I", first4)[0]
        body = sys.stdin.buffer.read(size)
        if len(body) < size:
            raise ValueError(f"short framed request body: got {len(body)} expected {size}")
        return body.decode("utf-8"), True

    # Raw JSON-RPC-over-stdio mode (newline/EOF terminated).
    rest = sys.stdin.buffer.read()
    return (first4 + rest).decode("utf-8"), False


def main():
    try:
        raw, framed = _read_request()
    except Exception as err:
        _log(f"read_request failed err={err!r}\n{traceback.format_exc()}")
        write_error(None, ERR_PARSE_ERROR, "invalid framed request", "invalid_argument", framed=False)
        return

    if not raw or not raw.strip():
        write_error(None, ERR_PARSE_ERROR, "empty request", "invalid_argument", framed=bool(framed))
        return

    try:
        req = json.loads(raw.strip())
    except Exception:
        write_error(None, ERR_PARSE_ERROR, "invalid JSON", "invalid_argument", framed=bool(framed))
        return

    req_id = req.get("id")
    method = req.get("method")
    _log(f"request framed={framed} id={req_id} method={method}")

    if method == METHOD_DESCRIBE:
        write_result(
            req_id,
            {
                "protocol_version": PROTOCOL_VERSION,
                "tool_name": TOOL_NAME,
                "supported_methods": SUPPORTED_METHODS,
                "schema_digest": _events_digest(),
            },
            framed=framed,
        )
        return

    if method == METHOD_SCHEMA:
        write_result(req_id, _schema_result(), framed=framed)
        return

    # Raw datasources are parsed in-process; the runner never calls tool/invoke.
    write_error(
        req_id,
        ERR_METHOD_NOT_FOUND,
        f"method not supported by raw datasource: {method}",
        "invalid_argument",
        framed=framed,
    )


if __name__ == "__main__":
    try:
        main()
    except Exception as err:
        _log(f"fatal err={err!r}\n{traceback.format_exc()}")
        write_error(None, ERR_INTERNAL, "tool execution failure", "execution", framed=False)
