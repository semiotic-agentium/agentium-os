import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent } from "vue";
import { mount } from "@vue/test-utils";
import { useEventObservation } from "./useEventObservation";
import type { ConversationHistoryPage } from "../types/a2a";

const TRACE_DEBOUNCE_MS = 300;

type Listener = (ev: MessageEvent<string>) => void;

class MockEventSource {
  static instances: MockEventSource[] = [];
  private listeners = new Map<string, Listener>();
  onerror: (() => void) | null = null;
  url: string;

  constructor(url: string) {
    this.url = url;
    MockEventSource.instances.push(this);
  }

  addEventListener(type: string, fn: Listener): void {
    this.listeners.set(type, fn);
  }

  emit(type: string, data: unknown): void {
    const fn = this.listeners.get(type);
    if (!fn) return;
    fn({ data: JSON.stringify(data) } as MessageEvent<string>);
  }

  close(): void {
    /* no-op */
  }
}

const emptyHistoryPage: ConversationHistoryPage = {
  contextId: "ctx-debounce",
  version: "1",
  maxEventOrder: 0,
  items: [],
};

describe("useEventObservation SSE trace refresh debounce", () => {
  let obs: ReturnType<typeof useEventObservation>;

  beforeEach(() => {
    MockEventSource.instances = [];
    vi.useFakeTimers();
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
      contextId: "ctx-debounce",
      version: "v1",
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
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/conversation-history?")) {
        return { ok: true, json: async () => pageWithItem };
      }
      if (url.includes("/mermaid")) {
        return { ok: true, text: async () => "" };
      }
      return { ok: false };
    });
    vi.stubGlobal("fetch", fetchMock);

    await obs.loadContext("ctx-debounce", null);
    await vi.advanceTimersByTimeAsync(TRACE_DEBOUNCE_MS);
    expect(obs.messages.value).toHaveLength(1);

    const stream = MockEventSource.instances[MockEventSource.instances.length - 1]!;
    stream.emit("snapshot", pageWithItem);
    await vi.advanceTimersByTimeAsync(TRACE_DEBOUNCE_MS);
    expect(obs.messages.value).toHaveLength(1);
  });

  it("coalesces rapid SSE snapshot events into one trace refresh bump", async () => {
    await obs.loadContext("ctx-debounce", null);
    await vi.advanceTimersByTimeAsync(TRACE_DEBOUNCE_MS);
    const before = obs.traceRefreshGeneration.value;

    const stream = MockEventSource.instances[MockEventSource.instances.length - 1]!;
    stream.emit("snapshot", emptyHistoryPage);
    stream.emit("snapshot", emptyHistoryPage);
    stream.emit("snapshot", emptyHistoryPage);

    await vi.advanceTimersByTimeAsync(TRACE_DEBOUNCE_MS - 1);
    expect(obs.traceRefreshGeneration.value).toBe(before);

    await vi.advanceTimersByTimeAsync(1);
    expect(obs.traceRefreshGeneration.value).toBe(before + 1);
  });
});
