// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/**
 * Summaries for `system/internal_a2a` send_done replay payloads.
 * Only inter-agent signals belong in the UI (not end-user echoes or assistant replies to the user).
 */

export function summarizeSessionStepHeader(op: { kind: string }): string | null {
  const header = (op as { header?: unknown }).header;
  if (typeof header !== "string" || header.trim().length === 0) return null;
  const trimmed = header.trim();
  const quoted = trimmed.match(/^@\S+\s+\S+\s+"(.+)"(?:\s+\[[^\]]+\])?$/);
  return (quoted?.[1] ?? trimmed).trim();
}

/** True when send_done header text reads like coordinator↔specialist traffic, not an end-user reply. */
export function isInterAgentA2aHeaderSummary(text: string): boolean {
  const t = text.trim();
  if (t.length === 0) return false;
  const lower = t.toLowerCase();

  const routingMarkers =
    /\bdelegated\b/i.test(t) ||
    /\bnotion-agent\b/i.test(lower) ||
    /\bclickup-agent\b/i.test(lower) ||
    /\bslack-agent\b/i.test(lower) ||
    /\bgithub-agent\b/i.test(lower) ||
    /\bcoordinator\b/i.test(lower) ||
    /\bsending message to delegated agent\b/i.test(lower) ||
    /\bdiscovering agents\b/i.test(lower) ||
    /\bsearching notion\b/i.test(lower) ||
    /\bretrieving notion\b/i.test(lower) ||
    /\binvoking tool\b/i.test(lower) ||
    /@[0-9]+\s+[\w.-]+\/[\w.-]+/.test(t) ||
    /\b(?:system|support|a2a)\/[\w.-]+\b/i.test(t);

  if (routingMarkers) return true;

  const looksLikeAssistantAnswer =
    /^(here|this|the|based on|in summary|overall|i'?ve|i have|you can|your)\b/i.test(t) && t.length > 80;

  if (looksLikeAssistantAnswer) return false;

  return t.length <= 220;
}

type A2aPayloadText = { text: string; role?: string };

function collectA2aPayloadMessages(
  value: unknown,
  out: A2aPayloadText[],
  inheritedRole?: string,
): void {
  if (value == null) return;
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed.length > 0 && trimmed.toLowerCase() !== "null") {
      out.push({ text: trimmed, role: inheritedRole });
    }
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) collectA2aPayloadMessages(item, out, inheritedRole);
    return;
  }
  if (typeof value !== "object") return;

  const obj = value as Record<string, unknown>;
  const role =
    typeof obj.role === "string" && obj.role.trim().length > 0 ? obj.role : inheritedRole;
  const parts = obj.parts;
  if (Array.isArray(parts)) {
    for (const part of parts) {
      if (!part || typeof part !== "object") continue;
      const text = (part as { text?: unknown }).text;
      if (typeof text === "string") {
        const trimmed = text.trim();
        if (trimmed.length > 0 && trimmed.toLowerCase() !== "null") {
          out.push({ text: trimmed, role });
        }
      }
    }
  }
  collectA2aPayloadMessages(obj.message, out, role);
  collectA2aPayloadMessages(obj.status, out, role);
  collectA2aPayloadMessages(obj.status_update, out, role);
  collectA2aPayloadMessages(obj.statusUpdate, out, role);
  collectA2aPayloadMessages(obj.chunks, out, role);
}

export function summarizeSendDoneReplayPayload(payload: unknown): string | null {
  if (payload == null) return null;
  const messages: A2aPayloadText[] = [];
  collectA2aPayloadMessages(payload, messages);
  const interAgent = messages.filter((entry) => {
    const role = (entry.role ?? "").toLowerCase();
    return role.includes("system") || role.includes("tool");
  });
  const unique = [...new Set(interAgent.map((entry) => entry.text))];
  if (unique.length === 0) return null;
  const joined = unique.join("\n");
  return joined.length > 900 ? `${joined.slice(0, 897)}...` : joined;
}
