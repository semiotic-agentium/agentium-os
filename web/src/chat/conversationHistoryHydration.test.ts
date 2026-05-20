import { describe, expect, it } from "vitest";
import { ref } from "vue";
import type { ChatMessage, ConversationHistoryPage } from "../types/a2a";
import { applyConversationHistoryPage } from "./conversationHistoryHydration";

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
});
