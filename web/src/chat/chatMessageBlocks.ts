// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type {
  ChatMessage,
  DataContentBlock,
  Part,
  TextContentBlock,
} from "../types/a2a";

export function ensureContentBlocks(msg: ChatMessage): void {
  if (msg.contentBlocks) return;
  msg.contentBlocks = [];
  if (msg.text) {
    msg.contentBlocks.push({ type: "text", text: msg.text });
  }
}

export function syncMsgTextFromTextBlocks(msg: ChatMessage): void {
  const blocks = msg.contentBlocks ?? [];
  msg.text = blocks
    .filter((b): b is TextContentBlock => b.type === "text")
    .map((b) => b.text)
    .join("\n\n");
}

/** Push a new text block so each message/chunk is its own area (no concatenation). */
export function pushTextBlock(msg: ChatMessage, text: string): void {
  const blocks = msg.contentBlocks!;
  blocks.push({ type: "text", text });
  syncMsgTextFromTextBlocks(msg);
}

/** Map A2A wire parts to UI blocks (prose + optional structured data parts). */
export function partsToContentBlocks(parts: Part[]): Array<TextContentBlock | DataContentBlock> {
  const out: Array<TextContentBlock | DataContentBlock> = [];
  for (const p of parts) {
    const rawText = p.text;
    if (typeof rawText === "string" && rawText.trim() !== "") {
      out.push({ type: "text", text: rawText });
    }
    const mediaHint = p.media_type ?? p.mediaType;
    const hasData = p.data !== undefined;
    if (hasData || mediaHint) {
      out.push({
        type: "data",
        mediaType: mediaHint ?? "application/octet-stream",
        data: hasData ? p.data : null,
      });
    }
  }
  return out;
}

/** Append structured part blocks (text + data) in order; keeps msg.text as joined text blocks only. */
export function pushStructuredBlocks(
  msg: ChatMessage,
  blocks: Array<TextContentBlock | DataContentBlock>,
): void {
  const arr = msg.contentBlocks!;
  for (const b of blocks) {
    arr.push(b);
  }
  syncMsgTextFromTextBlocks(msg);
}
