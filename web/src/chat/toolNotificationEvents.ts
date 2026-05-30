// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type { ToolEvent, ToolNotificationBlock } from "../types/a2a";

export function stableJsonSignature(value: unknown): string {
  const visit = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(visit);
    if (v && typeof v === "object") {
      const obj = v as Record<string, unknown>;
      return Object.keys(obj)
        .sort()
        .reduce<Record<string, unknown>>((acc, key) => {
          acc[key] = visit(obj[key]);
          return acc;
        }, {});
    }
    return v;
  };
  return JSON.stringify(visit(value));
}

export function pushSystemNoticeEvent(
  block: ToolNotificationBlock,
  subtype: string,
  text: string,
): void {
  const last = block.events[block.events.length - 1];
  if (
    last?.kind === "system_notice" &&
    last.subtype === subtype &&
    (last.text ?? "") === text
  ) {
    return;
  }
  block.events.push({
    kind: "system_notice",
    subtype,
    text,
  });
}

export function pushTerminalResultEvent(
  block: ToolNotificationBlock,
  subtype: string,
  result: string,
): void {
  const last = block.events[block.events.length - 1];
  if (
    last?.kind === "terminal_result" &&
    last.subtype === subtype &&
    (last.result ?? "") === result
  ) {
    return;
  }
  block.events.push({
    kind: "terminal_result",
    subtype,
    result,
  });
}

export function pushReadReplayEvent(block: ToolNotificationBlock, text: string): void {
  const last = block.events[block.events.length - 1];
  if (
    last?.kind === "assistant_text" &&
    last.subtype === "read_output" &&
    (last.text ?? "") === text
  ) {
    return;
  }
  block.events.push({
    kind: "assistant_text",
    subtype: "read_output",
    text,
  });
}

export function pushSessionStepDetailEvent(block: ToolNotificationBlock, text: string): void {
  const last = block.events[block.events.length - 1];
  if (
    last?.kind === "assistant_text" &&
    last.subtype === "session_step_detail" &&
    (last.text ?? "") === text
  ) {
    return;
  }
  block.events.push({
    kind: "assistant_text",
    subtype: "session_step_detail",
    text,
  });
}

export function pushA2aCommsEvent(block: ToolNotificationBlock, text: string): void {
  const last = block.events[block.events.length - 1];
  if (
    last?.kind === "assistant_text" &&
    last.subtype === "a2a_comms" &&
    (last.text ?? "") === text
  ) {
    return;
  }
  block.events.push({
    kind: "assistant_text",
    subtype: "a2a_comms",
    text,
  });
}

export function pushExecutionErrorDetailEvent(block: ToolNotificationBlock, text: string): void {
  const last = block.events[block.events.length - 1];
  if (
    last?.kind === "assistant_text" &&
    last.subtype === "execution_error" &&
    (last.text ?? "") === text
  ) {
    return;
  }
  block.events.push({
    kind: "assistant_text",
    subtype: "execution_error",
    text,
  });
}

export function pushToolEventsDeduped(block: ToolNotificationBlock, events: ToolEvent[]): void {
  for (const ev of events) {
    const last = block.events[block.events.length - 1];
    if (last && stableJsonSignature(last) === stableJsonSignature(ev)) continue;
    block.events.push(ev);
  }
}

/** Derive `session_step_detail` rows from live `system_notice` payloads that carry step text in `data`. */
export function withSessionStepDetailEvents(events: ToolEvent[]): ToolEvent[] {
  const enriched: ToolEvent[] = [];
  for (const ev of events) {
    enriched.push(ev);
    if (ev.kind !== "system_notice") continue;
    const subtype = typeof ev.subtype === "string" ? ev.subtype.trim().toLowerCase() : "";
    if (subtype !== "session step" && subtype !== "fsm phase") continue;
    const rawData = (ev as Record<string, unknown>).data;
    if (typeof rawData !== "string") continue;
    const detail = rawData.trim();
    if (!detail) continue;
    enriched.push({
      kind: "assistant_text",
      subtype: "session_step_detail",
      text: detail,
    });
  }
  return enriched;
}
