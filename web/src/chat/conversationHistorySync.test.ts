import { ref } from "vue";
import { describe, expect, it, vi } from "vitest";
import { applyConversationHistoryIngress } from "./conversationHistorySync";
import type {
  ChatMessage,
  ConversationHistoryItem,
  ConversationHistoryPage,
  HistoryHydrateState,
} from "../types/a2a";

function basePage(
  version: string,
  items: ConversationHistoryItem[],
  overrides: Partial<ConversationHistoryPage> = {},
): ConversationHistoryPage {
  return {
    contextId: "ctx-sync-test",
    version,
    maxEventOrder: items.length ? Math.max(...items.map((i) => i.timestampMs)) : 0,
    items,
    ...overrides,
  };
}

function messageItem(
  role: string,
  text: string,
  anchor: string,
  ts = 1,
): ConversationHistoryItem {
  return {
    timestampMs: ts,
    activityAnchor: anchor,
    role,
    content: { type: "message", text },
  };
}

function makeDeps(msgs: ChatMessage[], initialVersion = "") {
  const messages = ref<ChatMessage[]>(msgs);
  let historyVersion = initialVersion;
  const hydrateStates: HistoryHydrateState[] = [];
  const scheduledRetries: unknown[] = [];

  const deps = {
    messages,
    getHistoryVersion: () => historyVersion,
    setHistoryVersion: (v: string) => {
      historyVersion = v;
    },
    setHydrateState: (s: HistoryHydrateState) => {
      hydrateStates.push(s);
    },
    setSelectedContextId: vi.fn(),
    setTaskId: vi.fn(),
    replaceLlmFromPage: vi.fn(),
    extendLlmFromPage: vi.fn(),
    scheduleHydrateRetry: () => {
      scheduledRetries.push(null);
    },
  };

  return { deps, getHistoryVersion: () => historyVersion, hydrateStates, scheduledRetries };
}

describe("applyConversationHistoryIngress", () => {
  it("full apply replaces transcript and advances hydrate state to ready", () => {
    const { deps, hydrateStates } = makeDeps([]);
    const page = basePage("v1", [messageItem("user", "hi", "u1", 10)]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "explicit_restore",
      page,
      respectDuplicateVersion: false,
      syncTaskIdFromPageBeforeDefer: true,
    });
    expect(effect).toEqual({ kind: "applied_full" });
    expect(deps.messages.value.some((m) => m.role === "user" && m.text === "hi")).toBe(true);
    expect(deps.replaceLlmFromPage).toHaveBeenCalledWith(page);
    expect(hydrateStates[hydrateStates.length - 1]).toBe("ready");
  });

  it("full defers when agent is streaming (streaming_or_input_required)", () => {
    const agent: ChatMessage = {
      id: "live",
      role: "agent",
      text: "",
      timestamp: new Date(),
      isStreaming: true,
      contentBlocks: [],
    };
    const { deps, hydrateStates, scheduledRetries } = makeDeps([agent]);
    const page = basePage("v2", [messageItem("user", "hi", "u1")]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "explicit_restore",
      page,
      respectDuplicateVersion: false,
      syncTaskIdFromPageBeforeDefer: true,
    });
    expect(effect).toEqual({ kind: "deferred", reason: "streaming_or_input_required" });
    expect(deps.replaceLlmFromPage).not.toHaveBeenCalled();
    expect(hydrateStates).toContain("skipped");
    expect(scheduledRetries.length).toBe(1);
  });

  it("explicit restore applies when only blocker is an empty streaming placeholder", () => {
    const msgs: ChatMessage[] = [
      {
        id: "user-msg-restore-1",
        role: "user",
        speakerKind: "human",
        text: "hi",
        timestamp: new Date(),
      },
      {
        id: "live",
        role: "agent",
        text: "",
        timestamp: new Date(),
        isStreaming: true,
        contentBlocks: [],
      },
    ];
    const { deps, hydrateStates, scheduledRetries } = makeDeps(msgs);
    const page = basePage("v-restore", [
      messageItem("user", "hi", "u1", 10),
      messageItem("assistant", "reply", "a1", 20),
    ]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "explicit_restore",
      page,
      respectDuplicateVersion: false,
      syncTaskIdFromPageBeforeDefer: true,
    });
    expect(effect).toEqual({ kind: "applied_full" });
    expect(deps.messages.value.some((m) => m.role === "user" && m.text === "hi")).toBe(true);
    expect(deps.messages.value.some((m) => m.role === "agent" && m.text.includes("reply"))).toBe(
      true,
    );
    expect(hydrateStates[hydrateStates.length - 1]).toBe("ready");
    expect(scheduledRetries.length).toBe(0);
  });

  it("full defers when provenance lags streamed assistant body", () => {
    const agent: ChatMessage = {
      id: "live",
      role: "agent",
      text: "already streamed",
      timestamp: new Date(),
      contentBlocks: [],
    };
    const { deps, hydrateStates, scheduledRetries } = makeDeps([agent]);
    const page = basePage("v3", [], {});
    const effect = applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "explicit_restore",
      page,
      respectDuplicateVersion: false,
      syncTaskIdFromPageBeforeDefer: true,
    });
    expect(effect).toEqual({ kind: "deferred", reason: "provenance_lags_live" });
    expect(hydrateStates).toContain("skipped");
    expect(scheduledRetries.length).toBe(1);
  });

  it("duplicate server version is a no-op when respectDuplicateVersion is true", () => {
    const { deps } = makeDeps([], "same");
    const page = basePage("same", []);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "evented",
      page,
    });
    expect(effect).toEqual({ kind: "noop_duplicate_version" });
    expect(deps.replaceLlmFromPage).not.toHaveBeenCalled();
  });

  it("delta applies incremental items when live agent is idle", () => {
    const agent: ChatMessage = {
      id: "prov-agent",
      role: "agent",
      text: "",
      timestamp: new Date(),
      contentBlocks: [],
    };
    const { deps, hydrateStates } = makeDeps([agent]);
    const page = basePage("v-d1", [messageItem("assistant", "tail", "a1", 99)]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "delta",
      mode: "evented",
      page,
    });
    expect(effect).toEqual({ kind: "applied_delta" });
    expect(deps.extendLlmFromPage).toHaveBeenCalledWith(page);
    const last = deps.messages.value[deps.messages.value.length - 1];
    expect(last?.role).toBe("agent");
    expect(last?.text).toContain("tail");
    expect(hydrateStates[hydrateStates.length - 1]).toBe("ready");
  });

  it("delta still applies when assistant text is live but batch has no assistant message (no lag defer)", () => {
    const agent: ChatMessage = {
      id: "live",
      role: "agent",
      text: "streamed already",
      timestamp: new Date(),
      contentBlocks: [],
    };
    const { deps } = makeDeps([agent]);
    const page = basePage("v-lag-delta", [messageItem("user", "follow-up", "u3")]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "delta",
      mode: "evented",
      page,
    });
    expect(effect).toEqual({ kind: "applied_delta" });
    expect(deps.extendLlmFromPage).toHaveBeenCalledWith(page);
  });

  it("delta merges structural rows while agent is streaming (Primary tracks Observe)", () => {
    const agent: ChatMessage = {
      id: "live",
      role: "agent",
      text: "",
      timestamp: new Date(),
      isStreaming: true,
      contentBlocks: [],
    };
    const { deps, hydrateStates } = makeDeps([agent]);
    const page = basePage("v-d2-stream", [
      messageItem("user", "should-skip-in-structural", "u2", 2),
      {
        timestampMs: 3,
        activityAnchor: "tc1",
        role: "assistant",
        content: {
          type: "tool_call",
          tool_name: "support/notion",
          args: { x: 1 },
          fsm_phase: "active",
        },
      },
    ]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "delta",
      mode: "evented",
      page,
    });
    expect(effect).toEqual({ kind: "applied_delta" });
    expect(deps.extendLlmFromPage).toHaveBeenCalledWith(page);
    expect(deps.messages.value).toHaveLength(1);
    const last = deps.messages.value[0]!;
    expect(last.role).toBe("agent");
    expect(last.contentBlocks?.some((b) => b.type === "tool")).toBe(true);
    expect(hydrateStates[hydrateStates.length - 1]).toBe("ready");
  });

  it("evented full defers while A2A POST stream is in flight (guard initial snapshot)", () => {
    const { deps, hydrateStates } = makeDeps([
      {
        id: "user-msg-1",
        role: "user",
        speakerKind: "human",
        text: "hi",
        timestamp: new Date(),
      },
    ]);
    const depsWithA2a = {
      ...deps,
      deferFullSnapshotWhileA2aInFlight: () => true,
    };
    const page = basePage("v-snap", [messageItem("user", "hi", "u1", 10)]);
    const effect = applyConversationHistoryIngress(depsWithA2a, {
      kind: "full",
      mode: "evented",
      page,
    });
    expect(effect).toEqual({ kind: "deferred", reason: "streaming_or_input_required" });
    expect(deps.messages.value).toHaveLength(1);
    expect(hydrateStates).not.toContain("skipped");
  });

  it("delta structural mode skips assistant message text while streaming", () => {
    const agent: ChatMessage = {
      id: "live",
      role: "agent",
      text: "",
      timestamp: new Date(),
      isStreaming: true,
      contentBlocks: [],
    };
    const { deps } = makeDeps([agent]);
    const page = basePage("v-d-msg", [messageItem("assistant", "from provenance only", "a99", 5)]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "delta",
      mode: "evented",
      page,
    });
    expect(effect).toEqual({ kind: "applied_delta" });
    const last = deps.messages.value[0]!;
    const textBlocks = last.contentBlocks?.filter((b) => b.type === "text") ?? [];
    expect(textBlocks.length).toBe(0);
  });

  it("evented full defer does not schedule hydrate retry", () => {
    const agent: ChatMessage = {
      id: "live",
      role: "agent",
      text: "",
      timestamp: new Date(),
      isStreaming: true,
      contentBlocks: [],
    };
    const { deps, scheduledRetries } = makeDeps([agent]);
    applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "evented",
      page: basePage("v-e", []),
    });
    expect(scheduledRetries.length).toBe(0);
  });

  it("full defers when GET omits a client user-msg turn but still has older assistant text", () => {
    const msgs: ChatMessage[] = [
      {
        id: "user-msg-99-1",
        role: "user",
        speakerKind: "human",
        text: "follow-up",
        timestamp: new Date(),
      },
      {
        id: "agent-1",
        role: "agent",
        text: "fresh reply body",
        timestamp: new Date(),
        contentBlocks: [],
      },
    ];
    const { deps, scheduledRetries } = makeDeps(msgs);
    const page = basePage("v-stale-user", [
      messageItem("user", "earlier", "u-old", 10),
      messageItem("assistant", "earlier assistant row", "a-old", 20),
    ]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "background",
      page,
      respectDuplicateVersion: false,
      syncTaskIdFromPageBeforeDefer: true,
    });
    expect(effect).toEqual({ kind: "deferred", reason: "provenance_lags_live" });
    expect(deps.messages.value).toEqual(msgs);
    expect(scheduledRetries.length).toBe(1);
  });

  it("full applies when GET includes text for every client user-msg turn", () => {
    const msgs: ChatMessage[] = [
      {
        id: "user-msg-100-1",
        role: "user",
        speakerKind: "human",
        text: "hello",
        timestamp: new Date(),
      },
      {
        id: "agent-1",
        role: "agent",
        text: "reply",
        timestamp: new Date(),
        contentBlocks: [],
      },
    ];
    const { deps } = makeDeps(msgs);
    const page = basePage("v-synced-user", [
      messageItem("user", "hello", "u1", 10),
      messageItem("assistant", "reply", "a1", 20),
    ]);
    const effect = applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "background",
      page,
      respectDuplicateVersion: false,
      syncTaskIdFromPageBeforeDefer: true,
    });
    expect(effect).toEqual({ kind: "applied_full" });
    expect(deps.messages.value.some((m) => m.role === "user" && m.text === "hello")).toBe(true);
  });

  it("background full defer does not set skipped but still retries when scheduled", () => {
    const agent: ChatMessage = {
      id: "live",
      role: "agent",
      text: "",
      timestamp: new Date(),
      isStreaming: true,
      contentBlocks: [],
    };
    const { deps, hydrateStates, scheduledRetries } = makeDeps([agent]);
    applyConversationHistoryIngress(deps, {
      kind: "full",
      mode: "background",
      page: basePage("v-bg", []),
      respectDuplicateVersion: false,
      syncTaskIdFromPageBeforeDefer: true,
    });
    expect(hydrateStates).not.toContain("skipped");
    expect(scheduledRetries.length).toBe(1);
  });
});
