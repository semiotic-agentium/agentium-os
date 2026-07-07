// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** User message sent when operator denies tier-3 gate authorization. */
export const GATE_DENY_MESSAGE = "[gate-deny] Authorization denied by operator.";

export interface GateAuthorizationSummary {
  tier: number;
  groundedIntent: string;
  postconditionCount: number;
  rawPrompt: string;
}

const PROMPT_RE =
  /^Tier-(\d+) authorization required\.\s*\n+\s*Grounded intent:\s*(.+?)\s*\n+Postconditions declared:\s*(\d+)/s;

export function parseGateAuthorizationPrompt(prompt: string): GateAuthorizationSummary | null {
  const trimmed = prompt.trim();
  if (!trimmed) return null;
  const match = PROMPT_RE.exec(trimmed);
  if (!match) {
    return {
      tier: 3,
      groundedIntent: trimmed,
      postconditionCount: 0,
      rawPrompt: trimmed,
    };
  }
  return {
    tier: Number.parseInt(match[1] ?? "3", 10),
    groundedIntent: match[2]?.trim() ?? "",
    postconditionCount: Number.parseInt(match[3] ?? "0", 10),
    rawPrompt: trimmed,
  };
}

export function isGateAuthorizationMetadata(
  metadata: Record<string, unknown> | undefined | null,
): boolean {
  return metadata?.gateAuthorization === true;
}

/** Detect tier-3 host authorization prompt (conversation restore + stream). */
export function isGateAuthorizationPrompt(prompt: string | null | undefined): boolean {
  if (!prompt?.trim()) return false;
  return /^Tier-\d+ authorization required\./i.test(prompt.trim());
}

export function extractMessageMetadata(
  message: { metadata?: Record<string, unknown> } | undefined | null,
): Record<string, unknown> | undefined {
  if (!message?.metadata || typeof message.metadata !== "object") return undefined;
  return message.metadata;
}
