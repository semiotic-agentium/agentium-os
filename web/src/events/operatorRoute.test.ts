import { describe, expect, it } from "vitest";
import {
  readChatRouteFromUrl,
  readEventConsoleRouteFromUrl,
  writeChatRouteToUrl,
  writeEventConsoleRouteToUrl,
} from "./operatorRoute";

describe("operatorRoute", () => {
  it("writeEventConsoleRoute uses event-scoped query keys", () => {
    const original = window.location.href;
    writeEventConsoleRouteToUrl({
      agentPackage: "clickup-agent",
      agentInstance: "default",
      contextId: "ctx-ingress",
    });
    const params = new URLSearchParams(window.location.search);
    expect(params.get("view")).toBe("events");
    expect(params.get("eventAgentPackage")).toBe("clickup-agent");
    expect(params.get("eventAgentInstance")).toBe("default");
    expect(params.get("eventContextId")).toBe("ctx-ingress");
    expect(params.get("agentPackage")).toBeNull();
    window.history.replaceState(null, "", original);
  });

  it("readEventConsoleRoute falls back to legacy keys on events view", () => {
    const original = window.location.href;
    const url = new URL(window.location.href);
    url.search = "?view=events&agentPackage=coordinator-agent&agentInstance=default";
    window.history.replaceState(null, "", url.toString());
    expect(readEventConsoleRouteFromUrl()).toEqual({
      agentPackage: "coordinator-agent",
      agentInstance: "default",
      contextId: null,
    });
    window.history.replaceState(null, "", original);
  });

  it("readChatRoute does not use legacy keys on events view", () => {
    const original = window.location.href;
    const url = new URL(window.location.href);
    url.search = "?view=events&agentPackage=coordinator-agent";
    window.history.replaceState(null, "", url.toString());
    expect(readChatRouteFromUrl().agentPackage).toBeNull();
    window.history.replaceState(null, "", original);
  });

  it("writeChatRouteToUrl is a no-op when view is not chat", () => {
    const original = window.location.href;
    const url = new URL(window.location.href);
    url.search = "?view=events&eventAgentPackage=clickup-agent";
    window.history.replaceState(null, "", url.toString());
    writeChatRouteToUrl({
      agentPackage: "slack-agent",
      agentInstance: "default",
      contextId: "ctx-chat",
    });
    const params = new URLSearchParams(window.location.search);
    expect(params.get("view")).toBe("events");
    expect(params.get("chatAgentPackage")).toBeNull();
    expect(params.get("eventAgentPackage")).toBe("clickup-agent");
    window.history.replaceState(null, "", original);
  });

  it("writeChatRouteToUrl sets chat-scoped keys on chat view", () => {
    const original = window.location.href;
    const url = new URL(window.location.href);
    url.search = "?view=chat";
    window.history.replaceState(null, "", url.toString());
    writeChatRouteToUrl({
      agentPackage: "extrospection-agent",
      agentInstance: "default",
      contextId: "ctx-1",
    });
    const params = new URLSearchParams(window.location.search);
    expect(params.get("view")).toBe("chat");
    expect(params.get("chatAgentPackage")).toBe("extrospection-agent");
    expect(params.get("chatContextId")).toBe("ctx-1");
    expect(params.get("agentPackage")).toBeNull();
    window.history.replaceState(null, "", original);
  });
});
