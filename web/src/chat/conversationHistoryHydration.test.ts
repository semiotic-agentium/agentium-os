// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { ref } from "vue";
import type { ChatMessage, ConversationHistoryPage } from "../types/a2a";
import {
  applyConversationHistoryDelta,
  applyConversationHistoryPage,
  ConversationHistoryDeltaApplyMode,
  syncResumeHintsFromPage,
} from "./conversationHistoryHydration";

describe("applyConversationHistoryPage user speaker kind", () => {
  it("maps API userSpeakerKind onto ChatMessage.speakerKind", () => {
    const messages = ref<ChatMessage[]>([]);
    const page: ConversationHistoryPage = {
      contextId: "ctx-1",
      version: "v1",
      maxEventOrder: 1,
      items: [
        {
          timestampMs: 1,
          activityAnchor: "ingress-poll-user:ctx:msg",
          role: "user",
          userSpeakerKind: "ingress",
          content: { type: "message", text: "from poll" },
        },
        {
          timestampMs: 2,
          activityAnchor: "prov-2",
          role: "user",
          userSpeakerKind: "relay",
          content: { type: "message", text: "delegated" },
        },
        {
          timestampMs: 3,
          activityAnchor: "prov-3",
          role: "user",
          content: { type: "message", text: "human turn" },
        },
      ],
    };
    applyConversationHistoryPage(messages, page);
    expect(messages.value[0]?.speakerKind).toBe("ingress");
    expect(messages.value[1]?.speakerKind).toBe("relay");
    expect(messages.value[2]?.speakerKind).toBe("human");
  });

  it("maps tool_result error outcome to INTERRUPTED completion", () => {
    const messages = ref<ChatMessage[]>([]);
    const page: ConversationHistoryPage = {
      contextId: "ctx-1",
      version: "v1",
      maxEventOrder: 2,
      items: [
        {
          timestampMs: 1,
          activityAnchor: "tool-1",
          role: "tool",
          content: {
            type: "tool_call",
            tool_name: "support/calculate",
            args: { x: 1 },
            fsm_phase: "execute",
          },
        },
        {
          timestampMs: 2,
          activityAnchor: "tool-1",
          role: "tool",
          content: {
            type: "tool_result",
            tool_name: "support/calculate",
            fsm_phase: "execute",
            outcome: { kind: "error", value: "timeout while calling tool" },
          },
        },
      ],
    };
    applyConversationHistoryPage(messages, page);
    const toolBlock = messages.value[0]?.contentBlocks?.find((b) => b.type === "tool");
    expect(toolBlock?.type).toBe("tool");
    if (toolBlock?.type === "tool") {
      expect(toolBlock.completion).toBe("INTERRUPTED");
      expect(toolBlock.events.some((e) => e.kind === "terminal_result" && e.subtype === "error")).toBe(
        true,
      );
    }
  });

  it("hydrates operational_event rows as host/system cards", () => {
    const messages = ref<ChatMessage[]>([]);
    const page: ConversationHistoryPage = {
      contextId: "ctx-1",
      version: "v1",
      maxEventOrder: 1,
      items: [
        {
          timestampMs: 1,
          activityAnchor: "op-1",
          role: "host",
          content: {
            type: "operational_event",
            kind: "dispatch_rejected",
            severity: "error",
            summary: "Host dispatch rejected: pkg/default",
            detail: "no handler",
            agent_package: "pkg",
            agent_instance_id: "default",
          },
        },
      ],
    };
    applyConversationHistoryPage(messages, page);
    expect(messages.value[0]?.speakerKind).toBe("host");
    expect(messages.value[0]?.contentBlocks?.[0]?.type).toBe("operational");
  });

  it("upserts operational_event rows when the same delta is applied twice", () => {
    const messages = ref<ChatMessage[]>([]);
    const page: ConversationHistoryPage = {
      contextId: "ctx-1",
      version: "v1",
      maxEventOrder: 1,
      items: [
        {
          timestampMs: 1,
          activityAnchor: "host-ingress:op-dedupe",
          role: "host",
          content: {
            type: "operational_event",
            kind: "dispatch_rejected",
            severity: "error",
            summary: "Host dispatch rejected",
            detail: "rejected",
            agent_package: "pkg",
            agent_instance_id: "default",
          },
        },
      ],
    };
    applyConversationHistoryDelta(
      messages,
      page,
      ConversationHistoryDeltaApplyMode.Full,
    );
    applyConversationHistoryDelta(
      messages,
      page,
      ConversationHistoryDeltaApplyMode.Full,
    );
    expect(messages.value).toHaveLength(1);
    expect(messages.value[0]?.id).toBe("prov-op-host-ingress:op-dedupe");
  });
});

describe("syncResumeHintsFromPage gate authorization", () => {
  it("flags tier-3 authorization prompt on restore", () => {
    const messages = ref<ChatMessage[]>([
      {
        id: "a1",
        role: "agent",
        text: "Working…",
        timestamp: new Date(),
      },
    ]);
    syncResumeHintsFromPage(messages, {
      contextId: "ctx-1",
      version: "v1",
      maxEventOrder: 1,
      items: [],
      awaitingInput: true,
      inputRequiredPrompt:
        "Tier-3 authorization required.\n\nGrounded intent: Delete prod rows\nPostconditions declared: 1",
    });
    expect(messages.value[0]?.awaitingInput).toBe(true);
    expect(messages.value[0]?.gateAuthorization).toBe(true);
    expect(messages.value[0]?.inputRequiredPrompt).toContain("Grounded intent");
  });
});
