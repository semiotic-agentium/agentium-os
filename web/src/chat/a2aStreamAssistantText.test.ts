// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  collectChunkAssistantPlainText,
  extractWireMessageText,
} from "./a2aStreamAssistantText";
import type { ChunkPayload } from "../types/a2a";

describe("extractWireMessageText", () => {
  it("joins text parts", () => {
    const t = extractWireMessageText({
      messageId: "m",
      role: "agent",
      parts: [{ text: "a" }, { text: "b" }],
    });
    expect(t).toBe("a\n\nb");
  });

  it("reads raw part when text absent", () => {
    const t = extractWireMessageText({
      messageId: "m",
      role: "agent",
      parts: [{ raw: "raw-body" } as { text?: string; raw?: string }],
    });
    expect(t).toBe("raw-body");
  });

  it("stringifies JSON data parts", () => {
    const t = extractWireMessageText({
      messageId: "m",
      role: "agent",
      parts: [{ media_type: "application/json", data: { ok: true } }],
    });
    expect(t).toBe('{"ok":true}');
  });
});

describe("collectChunkAssistantPlainText", () => {
  it("prefers chunk.message then task.status.message", () => {
    const chunk: ChunkPayload = {
      message: {
        messageId: "1",
        role: "agent",
        parts: [{ text: "from message" }],
      },
      task: {
        status: {
          message: {
            messageId: "2",
            role: "agent",
            parts: [{ text: "ignored" }],
          },
        },
      },
    };
    expect(collectChunkAssistantPlainText(chunk)).toBe("from message");
  });

  it("reads nested statusUpdate.status.message", () => {
    const chunk: ChunkPayload = {
      statusUpdate: {
        statusUpdate: {
          status: {
            message: {
              messageId: "x",
              role: "agent",
              parts: [{ text: "nested" }],
            },
          },
        },
      },
    };
    expect(collectChunkAssistantPlainText(chunk)).toBe("nested");
  });

  it("reads task.history tail", () => {
    const chunk: ChunkPayload = {
      task: {
        history: [
          { messageId: "a", role: "user", parts: [{ text: "u" }] },
          { messageId: "b", role: "agent", parts: [{ text: "tail reply" }] },
        ],
      },
    };
    expect(collectChunkAssistantPlainText(chunk)).toBe("tail reply");
  });

  it("reads relay inner chunk.message", () => {
    const chunk: ChunkPayload = {
      chunk: {
        message: {
          messageId: "r",
          role: "agent",
          parts: [{ text: "relay prose" }],
        },
      },
    };
    expect(collectChunkAssistantPlainText(chunk)).toBe("relay prose");
  });
});
