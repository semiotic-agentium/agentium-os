#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

"""Normalize Doc Sync LLM output to a JSON object or NO_CHANGES_NEEDED."""

from __future__ import annotations

import json
import re
import sys
from typing import Any


def _strip_noise(text: str) -> str:
    return text.strip()


def _is_no_changes(text: str) -> bool:
    normalized = re.sub(r"\s+", "", text.strip())
    return normalized.upper() == "NO_CHANGES_NEEDED"


def _extract_fenced_json(text: str) -> str | None:
    match = re.search(r"```(?:json)?\s*(\{.*)\s*```", text, flags=re.DOTALL | re.IGNORECASE)
    if not match:
        return None
    return match.group(1).strip()


def _extract_outer_object(text: str) -> str | None:
    start = text.find("{")
    if start < 0:
        return None
    slice_ = text[start:]
    depth = 0
    in_string = False
    escape = False
    for index, char in enumerate(slice_):
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return slice_[: index + 1]
    return None


def parse_doc_sync_response(raw: str) -> dict[str, Any] | None:
    """Return a file→content map, or None when the model reports no updates."""
    text = _strip_noise(raw)
    if not text:
        raise ValueError("empty response")

    if _is_no_changes(text):
        return None

    candidates: list[str] = [text]
    fenced = _extract_fenced_json(text)
    if fenced:
        candidates.insert(0, fenced)
    outer = _extract_outer_object(text)
    if outer and outer not in candidates:
        candidates.insert(0, outer)

    last_error: json.JSONDecodeError | None = None
    for candidate in candidates:
        try:
            value = json.loads(candidate)
        except json.JSONDecodeError as exc:
            last_error = exc
            continue
        if not isinstance(value, dict):
            raise ValueError("expected JSON object mapping paths to file contents")
        if not all(isinstance(key, str) and isinstance(content, str) for key, content in value.items()):
            raise ValueError("expected string keys and string values")
        return value

    if last_error is not None:
        raise ValueError(f"could not parse JSON object: {last_error}") from last_error
    raise ValueError("expected JSON object or NO_CHANGES_NEEDED")


def main() -> int:
    if len(sys.argv) == 2 and sys.argv[1] == "--self-test":
        assert parse_doc_sync_response("NO_CHANGES_NEEDED") is None
        assert parse_doc_sync_response('  NO_CHANGES_NEEDED\n') is None
        parsed = parse_doc_sync_response('{"CLAUDE.md":"# ok\\n"}')
        assert parsed == {"CLAUDE.md": "# ok\n"}
        prose = (
            "Here is the update:\n\n```json\n"
            '{"docs/reference/agent-runner.md":"# runner\\n"}\n'
            "```\n"
        )
        assert parse_doc_sync_response(prose) == {
            "docs/reference/agent-runner.md": "# runner\n"
        }
        outer = 'Preamble text {"CLAUDE.md":"# x\\n"} trailing'
        assert parse_doc_sync_response(outer) == {"CLAUDE.md": "# x\n"}
        print("ok")
        return 0

    raw = sys.stdin.read()
    try:
        parsed = parse_doc_sync_response(raw)
    except ValueError as exc:
        print(f"parse-doc-sync-response: {exc}", file=sys.stderr)
        return 1

    if parsed is None:
        print("NO_CHANGES_NEEDED")
        return 0

    json.dump(parsed, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
