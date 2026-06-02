// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Wire shape from GET /conversation-history tool_result.outcome */
export type ToolOutcomeWire =
  | { kind: "result"; value: unknown }
  | { kind: "error"; value: unknown }
  | { kind: "status_only" };

export function formatToolOutcomeValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined || value === null) return "";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export function parseToolOutcome(outcome: unknown): {
  kind: "result" | "error" | "status_only";
  detail: string;
} {
  if (!outcome || typeof outcome !== "object") {
    return { kind: "status_only", detail: "" };
  }
  const o = outcome as Record<string, unknown>;
  const kind = o.kind;
  if (kind === "error") {
    return { kind: "error", detail: formatToolOutcomeValue(o.value) };
  }
  if (kind === "result") {
    return { kind: "result", detail: formatToolOutcomeValue(o.value) };
  }
  return { kind: "status_only", detail: "" };
}
