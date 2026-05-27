import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import MessageBubble from "./MessageBubble.vue";
import { INGRESS_WIRE_BODY_DELIMITER } from "../events/ingressWireBody";
import type { ChatMessage } from "../types/a2a";

function ingressMessage(text: string): ChatMessage {
  return {
    id: "ingress-1",
    role: "user",
    speakerKind: "ingress",
    text,
    timestamp: new Date(),
  };
}

describe("MessageBubble operational cards", () => {
  it("does not repeat detail when summary already embeds the agent reason", () => {
    const wrapper = mount(MessageBubble, {
      props: {
        message: {
          id: "op-1",
          role: "agent",
          speakerKind: "host",
          text: "",
          timestamp: new Date(),
          contentBlocks: [
            {
              type: "operational",
              kind: "dispatch_rejected",
              severity: "error",
              summary:
                "Host dispatch rejected: event:intake → slack-agent/default — clarify channel",
              detail: "clarify channel",
              agentPackage: "slack-agent",
              agentInstanceId: "default",
            },
          ],
        },
      },
    });
    expect(wrapper.findAll(".operational-card__detail")).toHaveLength(0);
    expect(wrapper.text()).toContain("clarify channel");
  });
});

describe("MessageBubble ingress wire", () => {
  it("renders full-width ingress card with readable code block (not user bubble)", () => {
    const payload = `${INGRESS_WIRE_BODY_DELIMITER}\n{"records":[{"text":"hello"}]}`;
    const wrapper = mount(MessageBubble, {
      props: { message: ingressMessage(payload) },
    });
    expect(wrapper.find(".ingress-wire-card").exists()).toBe(true);
    expect(wrapper.find(".ingress-wire-pre").exists()).toBe(true);
    expect(wrapper.find(".bubble.user").exists()).toBe(false);
    expect(wrapper.text()).toContain("hello");
    const pre = wrapper.get(".ingress-wire-pre");
    const style = getComputedStyle(pre.element);
    expect(style.color).not.toBe("rgb(255, 255, 255)");
  });
});
