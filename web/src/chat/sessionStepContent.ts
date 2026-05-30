// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type { SessionStepOp } from "../types/a2a";

/** Human-readable step parameters for provenance `session_step` rows (not A2A replay). */
export function summarizeSessionStepContent(toolName: string, op: SessionStepOp): string | null {
  switch (op.kind) {
    case "open":
      return null;
    case "page_read": {
      const lines = [`offset: ${op.offset}`, `limit: ${op.limit}`];
      if (op.archive_ref) lines.unshift(`archive_ref: ${op.archive_ref}`);
      return lines.join("\n");
    }
    case "search_read": {
      const lines = [`grep: ${op.grep}`, `offset: ${op.offset}`, `limit: ${op.limit}`];
      if (op.archive_ref) lines.unshift(`archive_ref: ${op.archive_ref}`);
      return lines.join("\n");
    }
    case "send_done": {
      if (toolName === "system/internal_a2a") return null;
      const send = op as Extract<SessionStepOp, { kind: "send_done" }>;
      const lines = [];
      if (typeof send.header === "string" && send.header.trim()) {
        lines.push(`header: ${send.header.trim()}`);
      }
      if (typeof send.informed_by === "string" && send.informed_by.trim()) {
        lines.push(`informed_by: ${send.informed_by.trim()}`);
      }
      if (typeof send.archive_ref === "string" && send.archive_ref.trim()) {
        lines.push(`archive_ref: ${send.archive_ref.trim()}`);
      }
      return lines.length > 0 ? lines.join("\n") : null;
    }
    default: {
      const raw = op as Record<string, unknown>;
      const pairs = Object.entries(raw)
        .filter(([k, v]) => k !== "kind" && v !== null && v !== undefined)
        .map(([k, v]) => `${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`);
      return pairs.length > 0 ? pairs.join("\n") : null;
    }
  }
}
