// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type { ToolEvent } from "../types/a2a";

export type DisplayEvent = {
  kind: string;
  text: string;
  toolUse?: { name: string; detail: string };
  count?: number;
};

export type FsmStepState = {
  key: string;
  label: string;
  status: "done" | "active" | "pending";
};

export function parseToolNameParts(toolName: string): {
  displayName: string;
  ordinal: number;
  hasExplicitOrdinal: boolean;
} {
  const m = toolName.match(/^(.+) \d+$/);
  const ord = toolName.match(/^.+ (\d+)$/);
  return {
    displayName: m ? m[1]! : toolName,
    ordinal: ord ? Number.parseInt(ord[1]!, 10) : 1,
    hasExplicitOrdinal: Boolean(ord),
  };
}

export function toolUseSummary(ev: ToolEvent): { name: string; detail: string } | null {
  if (ev.kind !== "assistant_tool_use") return null;
  const name = (ev.name ?? "tool") as string;
  let detail = "";
  const clip = (value: string, max = 72): string =>
    value.length > max ? `${value.slice(0, max - 3)}...` : value;
  try {
    const input = ev.input;
    if (typeof input === "string") {
      const parsed = JSON.parse(input) as Record<string, unknown>;
      if (typeof parsed.step_id === "string") {
        const step = parsed.step_id;
        const plan = typeof parsed.plan_id === "string" ? parsed.plan_id : undefined;
        detail = plan ? `${clip(step)} (${clip(plan)})` : clip(step);
      } else if (typeof parsed.file_path === "string") {
        const file = parsed.file_path.split("/").pop() ?? parsed.file_path;
        detail = file;
      } else if (typeof parsed.description === "string") {
        detail = parsed.description;
      } else if (typeof parsed.command === "string") {
        detail = parsed.command;
      }
    } else if (input && typeof input === "object" && !Array.isArray(input)) {
      const o = input as Record<string, unknown>;
      if (typeof o.step_id === "string") {
        const step = o.step_id as string;
        const plan = typeof o.plan_id === "string" ? (o.plan_id as string) : undefined;
        detail = plan ? `${clip(step)} (${clip(plan)})` : clip(step);
      } else if (typeof o.file_path === "string")
        detail = (o.file_path as string).split("/").pop() ?? o.file_path;
      else if (typeof o.description === "string") detail = o.description as string;
      else if (typeof o.command === "string") detail = o.command as string;
    }
  } catch {
    /* ignore parse errors */
  }
  return { name, detail };
}

function normalizeBookkeepingLabel(label: string): string {
  return label
    .trim()
    .toLowerCase()
    .replace(/_/g, " ")
    .replace(/\s+/g, " ");
}

function isPureBookkeepingSystemLabel(label: string): boolean {
  const normalized = normalizeBookkeepingLabel(label);
  return (
    normalized === "fsm phase: execution session complete" ||
    normalized === "session step: open" ||
    normalized === "session step: send done" ||
    normalized === "session step: finish" ||
    normalized === "execution session complete" ||
    normalized === "complete" ||
    normalized === "open" ||
    normalized === "send done" ||
    normalized === "finish"
  );
}

function parseFsmLabel(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const match = trimmed.match(/^(?:Session step|FSM phase):\s*(.+)$/i);
  if (!match || !match[1]?.trim()) return null;
  return match[1].trim().toLowerCase();
}

function extractFsmStepKey(ev: ToolEvent): string | null {
  const rec = ev as Record<string, unknown>;
  const fields: unknown[] = [ev.subtype, ev.text, rec.data, rec.result];

  for (const field of fields) {
    const parsed = parseFsmLabel(field);
    if (parsed) return parsed;
  }

  if (ev.kind !== "system_notice") return null;
  const subtypeNormalized =
    typeof ev.subtype === "string" ? normalizeBookkeepingLabel(ev.subtype) : "";
  if (subtypeNormalized !== "session step" && subtypeNormalized !== "fsm phase") {
    return null;
  }

  for (const field of [ev.text, rec.data, rec.result]) {
    if (typeof field !== "string") continue;
    const trimmed = field.trim();
    if (!trimmed) continue;
    if (normalizeBookkeepingLabel(trimmed) === subtypeNormalized) continue;
    return trimmed.toLowerCase();
  }

  return subtypeNormalized;
}

function eventDisplay(ev: ToolEvent): DisplayEvent {
  if (ev.kind === "assistant_thinking" && typeof ev.thinking === "string") {
    return { kind: "thinking", text: ev.thinking.trim() };
  }
  if (ev.kind === "assistant_text" && typeof ev.text === "string") {
    if (ev.subtype === "read_output") {
      return { kind: "read", text: ev.text.trim() };
    }
    if (ev.subtype === "session_step_detail") {
      return { kind: "step_detail", text: ev.text.trim() };
    }
    if (ev.subtype === "a2a_comms") {
      return { kind: "comms_outbound", text: ev.text.trim() };
    }
    if (ev.subtype === "execution_error") {
      return { kind: "failure", text: ev.text.trim() };
    }
    return { kind: "text", text: ev.text.trim() };
  }
  if (ev.kind === "assistant_tool_use") {
    const summary = toolUseSummary(ev);
    return {
      kind: "tool_use",
      text: summary
        ? summary.detail
          ? `${summary.name}: ${summary.detail}`
          : summary.name
        : (ev.name ?? "tool"),
      toolUse: summary ?? undefined,
    };
  }
  if (ev.kind === "terminal_result") {
    const sub = ev.subtype ?? "done";
    return { kind: "terminal", text: sub === "success" ? "Complete" : sub };
  }
  if (ev.kind === "system_notice") {
    const raw = ev.subtype ?? ev.text ?? "Status";
    const phaseMatch = raw.match(/Calling model:[^(]+\((.+?)\)/);
    const toolMatch = raw.match(/Invoking tool: (.+)/);
    const label = phaseMatch
      ? phaseMatch[1]!
      : toolMatch
        ? `Tool: ${toolMatch[1]}`
        : raw.startsWith("System: ")
          ? raw.slice("System: ".length)
          : raw;
    return { kind: "system", text: label };
  }
  return { kind: ev.kind || "event", text: String(ev.kind || "event") };
}

/** Collapse consecutive identical system events; filter pure bookkeeping rows. */
export function buildDisplayEvents(events: ToolEvent[]): DisplayEvent[] {
  const collapsed: DisplayEvent[] = [];
  for (const raw of events) {
    const ev = eventDisplay(raw);
    if (
      (ev.kind === "system" || ev.kind === "text" || ev.kind === "terminal") &&
      isPureBookkeepingSystemLabel(ev.text)
    ) {
      continue;
    }
    const last = collapsed[collapsed.length - 1];
    if (last && last.kind === "system" && ev.kind === "system" && last.text === ev.text) {
      last.count = (last.count ?? 1) + 1;
    } else {
      collapsed.push({ ...ev, count: ev.kind === "system" ? 1 : undefined });
    }
  }
  return collapsed;
}

export function buildFsmSteps(events: ToolEvent[], blockStatus: string): FsmStepState[] {
  const seen = new Set<string>();
  const ordered: string[] = [];

  for (const ev of events) {
    const key = extractFsmStepKey(ev);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    ordered.push(key);
  }

  if (ordered.length === 0) return [];
  const activeIdx = ordered.length - 1;
  return ordered.map((key, idx) => ({
    key,
    label: key.replace(/_/g, " "),
    status:
      idx < activeIdx
        ? "done"
        : idx === activeIdx && blockStatus === "Running"
          ? "active"
          : idx === activeIdx
            ? "done"
            : "pending",
  }));
}
