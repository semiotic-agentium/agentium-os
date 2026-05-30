// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type { ChatMessage } from "../types/a2a";
import { ensureContentBlocks } from "./chatMessageBlocks";
import { deriveToolStatus, getOrCreateToolBlockForAppend } from "./toolBlocks";
import { pushExecutionErrorDetailEvent, pushSystemNoticeEvent } from "./toolNotificationEvents";

export function parseExecutionErrorText(text: string): string | null {
  const trimmed = text.trim();
  if (trimmed.length === 0) return null;

  const exactBracketed = trimmed.match(/^\[execution error:\s*([\s\S]+?)\]$/i);
  if (exactBracketed) {
    return exactBracketed[1]!.trim();
  }

  const marker = "[execution error:";
  const lower = trimmed.toLowerCase();
  const idx = lower.indexOf(marker);
  if (idx === -1) return null;
  let detail = trimmed.slice(idx + marker.length).trim();
  if (detail.endsWith("]")) {
    detail = detail.slice(0, -1).trim();
  }
  return detail.length > 0 ? detail : null;
}

export function appendExecutionErrorCard(msg: ChatMessage, rawText: string): boolean {
  const detail = parseExecutionErrorText(rawText);
  if (!detail) return false;
  ensureContentBlocks(msg);
  const block = getOrCreateToolBlockForAppend(msg, "Execution error", "end");
  pushSystemNoticeEvent(block, "Execution error", "Execution error");
  pushExecutionErrorDetailEvent(block, detail);
  block.completion = "INTERRUPTED";
  block.status = deriveToolStatus(block);
  return true;
}
