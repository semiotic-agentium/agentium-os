#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

"""Open-Meteo MCP server (stdio, JSON-RPC line-delimited).

Speaks the Model Context Protocol 2025-06-18 over stdin/stdout so it can be
driven by `baml-agent-runner` after operator approval via
`cargo agent-platform mcp enable meteo`. Exposes a single tool, `get_meteo`,
whose input schema is the same as the in-tree `dev/meteo-tool` external
tool — kept identical so the meteo-mcp-agent BAML prompts can be a near
mechanical copy of meteo-agent's.

Network access required: this server reaches the public Open-Meteo
endpoints just like the external tool. Run inside the runner's import-time
sandbox (no env_clear surprises) or directly via `python3 main.py`.
"""

import json
import os
import sys
import time
from datetime import datetime, timezone
from urllib.parse import urlencode
from urllib.request import Request, urlopen

PROTOCOL_VERSION = "2025-06-18"
SERVER_NAME = "meteo-mcp"
SERVER_VERSION = "0.1.0"

TOOL_NAME = "get_meteo"
TOOL_DESCRIPTION = "Accurate weather forecasts for any location via Open-Meteo."

GEOCODING_ENDPOINT = "https://geocoding-api.open-meteo.com/v1/search"
FORECAST_ENDPOINT = "https://api.open-meteo.com/v1/forecast"

DEBUG = os.environ.get("METEO_MCP_DEBUG", "1") not in ("0", "false", "False")

# JSON-RPC error codes the runner's handler.rs classifies on:
# -32700 parse, -32600 invalid request, -32601 method not found,
# -32602 invalid params, -32603 internal. PR5 maps -32601/-32602/-32600
# to LLM-correctable; everything else is hard execution failure.
ERR_PARSE = -32700
ERR_INVALID_REQUEST = -32600
ERR_METHOD_NOT_FOUND = -32601
ERR_INVALID_PARAMS = -32602
ERR_INTERNAL = -32603

# Input schema kept *byte-for-byte equivalent* to the external meteo-tool's
# tool-metadata.json#/schemas/input so the agent prompt logic can be reused.
INPUT_SCHEMA = {
    "type": "object",
    "properties": {
        "location_query": {
            "type": "string",
            "description": "Free-form place query (city, country, region), e.g. 'Buenos Aires' or 'Athens, Greece'.",
        },
        "city": {
            "type": "string",
            "description": "Optional city hint. Used when location_query is omitted.",
        },
        "country": {
            "type": "string",
            "description": "Optional country hint, e.g. 'Panama'.",
        },
        "timezone": {
            "type": "string",
            "description": "IANA timezone like Europe/Berlin, or auto.",
            "default": "auto",
        },
        "hourly_limit": {
            "type": "integer",
            "description": "Maximum number of hourly datapoints to return (1..48).",
            "minimum": 1,
            "maximum": 48,
            "default": 12,
        },
    },
    "required": ["location_query"],
    "additionalProperties": False,
}


def _log(msg: str) -> None:
    if not DEBUG:
        return
    now = datetime.now(timezone.utc).isoformat()
    print(f"[{now}] {SERVER_NAME} {msg}", file=sys.stderr, flush=True)


def _write_message(value: dict) -> None:
    sys.stdout.write(json.dumps(value) + "\n")
    sys.stdout.flush()


def _ok(req_id, result) -> None:
    _write_message({"jsonrpc": "2.0", "id": req_id, "result": result})


def _err(req_id, code: int, message: str) -> None:
    _write_message(
        {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
    )


def _http_get_json(url: str) -> dict:
    req = Request(
        url,
        headers={"Accept": "application/json", "User-Agent": f"{SERVER_NAME}/{SERVER_VERSION}"},
    )
    start = time.monotonic()
    _log(f"http start url={url}")
    try:
        with urlopen(req, timeout=15) as resp:
            body = resp.read().decode("utf-8")
            elapsed_ms = int((time.monotonic() - start) * 1000)
            _log(f"http ok status={getattr(resp, 'status', 'unknown')} elapsed_ms={elapsed_ms}")
            return json.loads(body)
    except Exception as err:
        elapsed_ms = int((time.monotonic() - start) * 1000)
        _log(f"http error elapsed_ms={elapsed_ms} err={err!r}")
        raise


def _as_int(value, key: str) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        raise ValueError(f"{key} must be an integer")


def _validate_input(input_obj: dict) -> dict:
    if not isinstance(input_obj, dict):
        raise ValueError("arguments must be an object")

    raw_location_query = input_obj.get("location_query", "")
    location_query = raw_location_query.strip() if isinstance(raw_location_query, str) else ""

    raw_city = input_obj.get("city", "")
    city = raw_city.strip() if isinstance(raw_city, str) else ""

    raw_country = input_obj.get("country", "")
    country = raw_country.strip() if isinstance(raw_country, str) else ""

    if not location_query:
        if city and country:
            location_query = f"{city}, {country}"
        elif city:
            location_query = city
        elif country:
            location_query = country

    if not location_query:
        raise ValueError("provide location_query, or city/country")

    raw_timezone = input_obj.get("timezone", "auto")
    if raw_timezone is None or raw_timezone == "":
        timezone_name = "auto"
    elif isinstance(raw_timezone, str):
        timezone_name = raw_timezone
    else:
        raise ValueError("timezone must be a string")

    raw_hourly_limit = input_obj.get("hourly_limit", 12)
    if raw_hourly_limit is None or raw_hourly_limit == "":
        raw_hourly_limit = 12
    hourly_limit = _as_int(raw_hourly_limit, "hourly_limit")
    if not 1 <= hourly_limit <= 48:
        raise ValueError("hourly_limit must be between 1 and 48")

    return {
        "location_query": location_query,
        "city": city or None,
        "country": country or None,
        "timezone": timezone_name,
        "hourly_limit": hourly_limit,
    }


def _resolve_location(location_query: str, country_hint):
    query_text = location_query
    if country_hint and country_hint.lower() not in location_query.lower():
        query_text = f"{location_query}, {country_hint}"

    query = {"name": query_text, "count": 5, "language": "en", "format": "json"}
    payload = _http_get_json(f"{GEOCODING_ENDPOINT}?{urlencode(query)}")

    results = payload.get("results", [])
    if not results:
        raise ValueError(f"no location found for '{location_query}'")

    best = results[0]
    candidates = []
    for row in results[:3]:
        candidates.append(
            {
                "name": row.get("name"),
                "admin1": row.get("admin1"),
                "country": row.get("country"),
                "latitude": row.get("latitude"),
                "longitude": row.get("longitude"),
            }
        )
    return best, candidates


def _fetch_forecast(latitude, longitude, timezone_name):
    query = {
        "latitude": latitude,
        "longitude": longitude,
        "current": "temperature_2m,wind_speed_10m",
        "hourly": "temperature_2m,relative_humidity_2m,wind_speed_10m",
        "timezone": timezone_name,
        "forecast_days": 1,
    }
    return _http_get_json(f"{FORECAST_ENDPOINT}?{urlencode(query)}")


def _zip_hourly(hourly: dict, limit: int):
    times = hourly.get("time", [])
    temps = hourly.get("temperature_2m", [])
    humidity = hourly.get("relative_humidity_2m", [])
    wind = hourly.get("wind_speed_10m", [])
    rows = []
    for i in range(min(len(times), len(temps), len(humidity), len(wind), limit)):
        rows.append(
            {
                "time": times[i],
                "temperature_2m": temps[i],
                "relative_humidity_2m": humidity[i],
                "wind_speed_10m": wind[i],
            }
        )
    return rows


def _call_get_meteo(arguments: dict) -> dict:
    normalized = _validate_input(arguments)
    best, candidates = _resolve_location(normalized["location_query"], normalized["country"])

    latitude = best.get("latitude")
    longitude = best.get("longitude")
    if latitude is None or longitude is None:
        raise ValueError("resolved location is missing coordinates")

    api_payload = _fetch_forecast(latitude, longitude, normalized["timezone"])
    current = api_payload.get("current", {})
    hourly = api_payload.get("hourly", {})

    structured = {
        "source": "open-meteo",
        "fetched_at": datetime.now(timezone.utc).isoformat(),
        "location": {
            "query": normalized["location_query"],
            "name": best.get("name"),
            "admin1": best.get("admin1"),
            "country": best.get("country"),
            "country_code": best.get("country_code"),
            "latitude": api_payload.get("latitude", latitude),
            "longitude": api_payload.get("longitude", longitude),
            "timezone": api_payload.get("timezone", normalized["timezone"]),
        },
        "current": {
            "time": current.get("time"),
            "temperature_2m": current.get("temperature_2m"),
            "wind_speed_10m": current.get("wind_speed_10m"),
        },
        "hourly": _zip_hourly(hourly, normalized["hourly_limit"]),
        "location_candidates": candidates,
    }

    # MCP CallToolResult: content[] is the LLM-facing payload, structuredContent
    # carries the typed body. The runner's `result_to_envelope` preserves both
    # so the BAML prompt can read either.
    summary = (
        f"weather for {structured['location']['name'] or normalized['location_query']}"
        f" ({structured['location']['country'] or '?'}): "
        f"{structured['current']['temperature_2m']}°, "
        f"wind {structured['current']['wind_speed_10m']}"
    )
    return {
        "content": [
            {"type": "text", "text": summary},
            {"type": "text", "text": json.dumps(structured)},
        ],
        "structuredContent": structured,
        "isError": False,
    }


def _tool_descriptor() -> dict:
    return {
        "name": TOOL_NAME,
        "description": TOOL_DESCRIPTION,
        "inputSchema": INPUT_SCHEMA,
    }


def _handle(message: dict) -> None:
    method = message.get("method", "")
    req_id = message.get("id")
    params = message.get("params") or {}
    _log(f"recv method={method} id={req_id!r}")

    if method == "initialize":
        _ok(
            req_id,
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            },
        )
        return

    if method == "notifications/initialized":
        # No response — notification.
        return

    if method == "tools/list":
        _ok(req_id, {"tools": [_tool_descriptor()]})
        return

    if method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments") or {}
        if name != TOOL_NAME:
            _err(req_id, ERR_METHOD_NOT_FOUND, f"unknown tool: {name}")
            return
        try:
            result = _call_get_meteo(arguments)
            _ok(req_id, result)
        except ValueError as err:
            # Invalid input — surface as tools/call result with isError=true
            # so the model gets a chance to correct. Alternative would be a
            # JSON-RPC error; rmcp accepts either.
            _ok(
                req_id,
                {
                    "content": [{"type": "text", "text": f"invalid input: {err}"}],
                    "isError": True,
                },
            )
        except Exception as err:  # noqa: BLE001 — surface as transport-class
            _log(f"call_tool exception: {err!r}")
            _err(req_id, ERR_INTERNAL, f"{type(err).__name__}: {err}")
        return

    if method == "shutdown":
        _ok(req_id, None)
        return

    if method == "exit":
        sys.exit(0)

    if method.startswith("notifications/"):
        # Unknown notifications are dropped silently per JSON-RPC spec.
        return

    _err(req_id, ERR_METHOD_NOT_FOUND, f"method not found: {method}")


def main() -> int:
    _log("ready on stdio")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as err:
            _err(None, ERR_PARSE, f"parse error: {err}")
            continue
        try:
            _handle(message)
        except Exception as err:  # noqa: BLE001 — never crash the loop
            _log(f"handler exception: {err!r}")
            _err(message.get("id"), ERR_INTERNAL, f"{type(err).__name__}: {err}")
    _log("stdin closed; exiting")
    return 0


if __name__ == "__main__":
    sys.exit(main())
