#!/usr/bin/env python3
"""Meteo external tool implementation (JSON-RPC over stdio)."""

import json
import os
import struct
import sys
import time
import traceback
from datetime import datetime, timezone
from urllib.parse import urlencode
from urllib.request import Request, urlopen

PROTOCOL_VERSION = "1"
METHOD_DESCRIBE = "tool/describe"
METHOD_INVOKE = "tool/invoke"
SUPPORTED_METHODS = [METHOD_DESCRIBE, METHOD_INVOKE]

ERR_METHOD_NOT_FOUND = -32601
ERR_PARSE_ERROR = -32700
ERR_INVALID_PARAMS = -32602
ERR_INTERNAL = -32000

TOOL_NAME = "dev/meteo-tool"
GEOCODING_ENDPOINT = "https://geocoding-api.open-meteo.com/v1/search"
FORECAST_ENDPOINT = "https://api.open-meteo.com/v1/forecast"
DEBUG = os.environ.get("METEO_TOOL_DEBUG", "1") not in ("0", "false", "False")


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
    if framed:
        _write_framed(payload)
    else:
        _write_raw(payload)


def write_error(req_id, code, message, error_class="execution", framed=False):
    payload = _encode_response(
        req_id=req_id,
        error={
            "code": code,
            "message": message,
            "data": {"error_class": error_class},
        },
    )
    if framed:
        _write_framed(payload)
    else:
        _write_raw(payload)


def _as_int(value, key):
    try:
        return int(value)
    except (TypeError, ValueError):
        raise ValueError(f"{key} must be an integer")


def _http_get_json(url):
    req = Request(url, headers={"Accept": "application/json", "User-Agent": "meteo-tool/0.2"})
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


def _validate_input(input_obj):
    if not isinstance(input_obj, dict):
        raise ValueError("input must be an object")

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


def _resolve_location(location_query, country_hint):
    query_text = location_query
    if country_hint and country_hint.lower() not in location_query.lower():
        query_text = f"{location_query}, {country_hint}"

    query = {
        "name": query_text,
        "count": 5,
        "language": "en",
        "format": "json",
    }
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


def _candidate_label(candidate):
    parts = [candidate.get("name"), candidate.get("admin1"), candidate.get("country")]
    return ", ".join([p for p in parts if p])


def _query_is_specific(location_query):
    if "," in location_query:
        return True

    tokens = [t for t in location_query.strip().split() if t]
    return len(tokens) >= 3


def _should_ask_clarification(location_query, country_hint, candidates):
    if country_hint:
        return False
    if len(candidates) < 2:
        return False
    if _query_is_specific(location_query):
        return False

    first = candidates[0]
    second = candidates[1]
    country_differs = (first.get("country") or "") != (second.get("country") or "")
    admin_differs = (first.get("admin1") or "") != (second.get("admin1") or "")
    return country_differs or admin_differs


def _maybe_raise_ambiguous(location_query, country_hint, candidates):
    if not _should_ask_clarification(location_query, country_hint, candidates):
        return

    options = [f"{idx + 1}) {_candidate_label(c)}" for idx, c in enumerate(candidates[:3])]
    options_text = "; ".join(options)
    raise ValueError(
        f"ambiguous location '{location_query}'. Please clarify by city and country. Options: {options_text}"
    )


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


def _zip_hourly(hourly, limit):
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


def _invoke(params):
    invoke_start = time.monotonic()
    _log(f"invoke start params_keys={list((params or {}).keys())}")

    normalized = _validate_input(params.get("input", {}))
    _log(f"invoke normalized_input={normalized}")

    best_location, candidates = _resolve_location(
        normalized["location_query"],
        normalized["country"],
    )
    _log(
        "invoke resolved_location "
        f"name={best_location.get('name')} country={best_location.get('country')} "
        f"lat={best_location.get('latitude')} lon={best_location.get('longitude')}"
    )

    _maybe_raise_ambiguous(normalized["location_query"], normalized["country"], candidates)

    latitude = best_location.get("latitude")
    longitude = best_location.get("longitude")
    if latitude is None or longitude is None:
        raise ValueError("resolved location is missing coordinates")

    api_payload = _fetch_forecast(latitude, longitude, normalized["timezone"])

    current = api_payload.get("current", {})
    hourly = api_payload.get("hourly", {})

    elapsed_ms = int((time.monotonic() - invoke_start) * 1000)
    _log(f"invoke done elapsed_ms={elapsed_ms}")

    return {
        "output": {
            "source": "open-meteo",
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "location": {
                "query": normalized["location_query"],
                "name": best_location.get("name"),
                "admin1": best_location.get("admin1"),
                "country": best_location.get("country"),
                "country_code": best_location.get("country_code"),
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
        },
        "done": True,
    }


def _read_request():
    first4 = sys.stdin.buffer.read(4)
    if not first4:
        _log("read_request: empty stdin")
        return None, False

    # TSRPC framed mode: 4-byte BE length + JSON body.
    if any((b < 32 and b not in (9, 10, 13)) for b in first4):
        size = struct.unpack(">I", first4)[0]
        _log(f"read_request: framed size={size}")
        body = sys.stdin.buffer.read(size)
        if len(body) < size:
            raise ValueError(f"short framed request body: got {len(body)} expected {size}")
        return body.decode("utf-8"), True

    # Raw JSON-RPC-over-stdio mode (newline/EOF terminated).
    rest = sys.stdin.buffer.read()
    raw = (first4 + rest).decode("utf-8")
    _log(f"read_request: raw bytes={len(raw.encode('utf-8'))}")
    return raw, False


def main():
    _log(f"main start pid={os.getpid()}")
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
    _log(f"parsed request framed={framed} id={req_id} method={method}")

    if method == METHOD_DESCRIBE:
        write_result(
            req_id,
            {
                "protocol_version": PROTOCOL_VERSION,
                "tool_name": TOOL_NAME,
                "supported_methods": SUPPORTED_METHODS,
            },
            framed=framed,
        )
        return

    if method == METHOD_INVOKE:
        try:
            result = _invoke(req.get("params", {}))
        except ValueError as err:
            _log(f"invoke value_error err={err!r}")
            write_error(req_id, ERR_INVALID_PARAMS, str(err), "invalid_argument", framed=framed)
            return
        except Exception as err:
            _log(f"invoke exception err={err!r}\n{traceback.format_exc()}")
            write_error(req_id, ERR_INTERNAL, "weather lookup failed", "execution", framed=framed)
            return

        write_result(req_id, result, framed=framed)
        return

    write_error(req_id, ERR_METHOD_NOT_FOUND, f"method not found: {method}", "invalid_argument", framed=framed)


if __name__ == "__main__":
    try:
        main()
    except Exception as err:
        _log(f"fatal err={err!r}\n{traceback.format_exc()}")
        write_error(None, ERR_INTERNAL, "tool execution failure", "execution", framed=False)
