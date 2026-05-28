import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent } from "vue";
import { mount } from "@vue/test-utils";
import { useEventObservation } from "./useEventObservation";
import type { ConversationHistoryPage } from "../types/a2a";

type Listener = (ev: MessageEvent<string>) => void;

class MockEventSource {
  static instances: MockEventSource[] = [];
  private listeners = new Map<string, Listener>();
  onerror: (() => void) | null = null;
  url: string;
  closed = false;

  constructor(url: string) {
    this.url = url;
    MockEventSource.instances.push(this);
  }

  addEventListener(type: string, fn: Listener): void {
    this.listeners.set(type, fn);
  }

  emit(type: string, data?: unknown): void {
    const fn = this.listeners.get(type);
    if (!fn) return;
    fn({ data: JSON.stringify(data ?? {}) } as MessageEvent<string>);
  }

  close(): void {
    this.closed = true;
  }

  static openInstances(): MockEventSource[] {
    return MockEventSource.instances.filter((s) => !s.closed);
  }
}

const emptyHistoryPage: ConversationHistoryPage = {
  contextId: "ctx-observe",
  version: "obs-v1:0",
  maxEventOrder: 0,
  items: [],
};

describe("useEventObservation transcript reconcile", () => {
  let obs: ReturnType<typeof useEventObservation>;

  beforeEach(() => {
    vi.useFakeTimers();
    MockEventSource.instances = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/conversation-history?")) {
          return { ok: true, json: async () => emptyHistoryPage };
        }
        if (url.includes("/mermaid")) {
          return { ok: true, text: async () => "sequenceDiagram\n  A->>B: x" };
        }
        return { ok: false };
      }),
    );
    vi.stubGlobal("EventSource", MockEventSource);

    const Harness = defineComponent({
      setup() {
        obs = useEventObservation();
        return () => null;
      },
    });
    mount(Harness);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("skips redundant SSE snapshots with the same version when transcript is loaded", async () => {
    const pageWithItem: ConversationHistoryPage = {
      contextId: "ctx-observe",
      version: "obs-v1:abc",
      maxEventOrder: 1,
      items: [
        {
          activityAnchor: "a1",
          role: "user",
          timestampMs: 1,
          content: { type: "message", text: "hello" },
        },
      ],
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/conversation-history?")) {
          return { ok: true, json: async () => pageWithItem };
        }
        if (url.includes("/mermaid")) {
          return { ok: true, text: async () => "" };
        }
        return { ok: false };
      }),
    );

    await obs.loadContext("ctx-observe", null);
    expect(obs.messages.value).toHaveLength(1);
    const before = obs.traceRefreshGeneration.value;

    const stream = MockEventSource.instances[MockEventSource.instances.length - 1]!;
    stream.emit("snapshot", pageWithItem);
    expect(obs.messages.value).toHaveLength(1);
    expect(obs.traceRefreshGeneration.value).toBe(before);
  });

  it("bumps trace refresh on each applied SSE snapshot with a new version", async () => {
    await obs.loadContext("ctx-observe", null);
    const before = obs.traceRefreshGeneration.value;

    const stream = MockEventSource.instances[MockEventSource.instances.length - 1]!;
    stream.emit("snapshot", { ...emptyHistoryPage, version: "obs-v1:1" });
    stream.emit("snapshot", { ...emptyHistoryPage, version: "obs-v1:2" });
    stream.emit("snapshot", { ...emptyHistoryPage, version: "obs-v1:3" });

    expect(obs.traceRefreshGeneration.value).toBe(before + 3);
  });

  it("reconciles SSE delta via authoritative GET merge (same path as reload)", async () => {
    const pageWithItem: ConversationHistoryPage = {
      contextId: "ctx-observe",
      version: "obs-v1:live",
      maxEventOrder: 2,
      items: [
        {
          activityAnchor: "ingress-1",
          role: "user",
          timestampMs: 1,
          content: { type: "message", text: "wire" },
        },
        {
          activityAnchor: "agent-1",
          role: "agent",
          timestampMs: 2,
          content: { type: "message", text: "ack" },
        },
      ],
    };

    let fetchCalls = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/conversation-history?")) {
          fetchCalls += 1;
          return { ok: true, json: async () => (fetchCalls === 1 ? emptyHistoryPage : pageWithItem) };
        }
        if (url.includes("/mermaid")) {
          return { ok: true, text: async () => "" };
        }
        return { ok: false };
      }),
    );

    await obs.loadContext("ctx-observe", null);
    expect(obs.messages.value).toHaveLength(0);

    const stream = MockEventSource.instances[MockEventSource.instances.length - 1]!;
    stream.emit("delta");
    await vi.advanceTimersByTimeAsync(80);

    expect(fetchCalls).toBeGreaterThanOrEqual(2);
    expect(obs.messages.value).toHaveLength(2);
    expect(obs.messages.value.map((m) => m.role)).toEqual(["user", "agent"]);
  });

  it("keeps one history stream when reloading the same observe scope", async () => {
    await obs.loadContext("ctx-observe", null);
    const streamsAfterFirst = MockEventSource.instances.length;

    await obs.loadContext("ctx-observe", null);
    expect(MockEventSource.instances.length).toBe(streamsAfterFirst);
  });

  it("opens one history stream when loadContext runs concurrently for the same scope", async () => {
    const pending = Promise.all([
      obs.loadContext("ctx-observe", "dispatch-unit-abc"),
      obs.loadContext("ctx-observe", "dispatch-unit-abc"),
    ]);
    await vi.runAllTimersAsync();
    await pending;

    const streams = MockEventSource.instances.filter((s) =>
      s.url.includes("taskId=dispatch-unit-abc"),
    );
    expect(streams).toHaveLength(1);
    expect(MockEventSource.openInstances()).toHaveLength(1);
  });

  it("closes context-only stream when taskId resolves on the same context", async () => {
    await obs.loadContext("ctx-observe", null);
    const contextOnly = MockEventSource.instances.find(
      (s) => s.url.includes("ctx-observe") && !s.url.includes("taskId="),
    );
    expect(contextOnly).toBeDefined();

    await obs.loadContext("ctx-observe", "dispatch-unit-abc");
    expect(contextOnly!.closed).toBe(true);

    const openTaskStreams = MockEventSource.openInstances().filter((s) =>
      s.url.includes("taskId=dispatch-unit-abc"),
    );
    expect(openTaskStreams).toHaveLength(1);
  });

  it("runs separate loadContext work when preserve mode differs", async () => {
    let fetchCalls = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/conversation-history?")) {
          fetchCalls += 1;
          return { ok: true, json: async () => emptyHistoryPage };
        }
        if (url.includes("/mermaid")) {
          return { ok: true, text: async () => "" };
        }
        return { ok: false };
      }),
    );

    await Promise.all([
      obs.loadContext("ctx-observe", "dispatch-unit-abc", {
        preserveMessagesUntilTranscript: true,
      }),
      obs.loadContext("ctx-observe", "dispatch-unit-abc"),
    ]);

    expect(fetchCalls).toBe(2);
    expect(MockEventSource.openInstances()).toHaveLength(1);
  });
});
