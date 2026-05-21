import { describe, expect, it, vi } from "vitest";
import type { AgentDiscoveryEntry } from "../types/a2a";
import type { AgentDeliverableMessageShape, EventDispatchScope } from "../types/events";
import {
  mergeContextPickerItems,
  resolveAgentFromRoute,
  resolveSubscribedAgentFromRoute,
  writeEventConsoleRoute,
} from "../composables/useEventConsole";
import {
  buildOperatorPublishTraceMessages,
  publishedScopeMatchesAgent,
  resolveObservationScope,
  scopeFromContextId,
  scopeFromDraftScope,
  scopeFromRecord,
  transcriptHasHostIngress,
} from "./dispatchObserve";
import { deriveDispatchEnvelope } from "./messageShapes";

function agent(
  pkg: string,
  inst: string,
  subscriptions: AgentDiscoveryEntry["agent_card"]["subscriptions"],
): AgentDiscoveryEntry {
  return {
    agent_package: pkg,
    agent_instance_id: inst,
    agent_card: { subscriptions },
  } as AgentDiscoveryEntry;
}

/** Mirror validate API scope encoding (snake_case tagged union). */
function scopeToJson(scope: EventDispatchScope): Record<string, unknown> {
  if (scope.kind === "new_context") return { kind: "new_context" };
  if (scope.kind === "existing_context") {
    return { kind: "existing_context", context_id: scope.context_id };
  }
  return {
    kind: "existing_task",
    context_id: scope.context_id,
    task_id: scope.task_id,
  };
}

describe("mergeContextPickerItems", () => {
  it("prefers recent rows and dedupes by contextId", () => {
    const merged = mergeContextPickerItems(
      [
        {
          contextId: "ctx-a",
          latestTimestampMs: 100,
          preview: "server a",
        },
      ],
      [
        {
          contextId: "ctx-b",
          latestTimestampMs: 200,
          preview: "recent b",
        },
        {
          contextId: "ctx-a",
          latestTimestampMs: 300,
          preview: "recent a",
        },
      ],
    );
    expect(merged.map((m) => m.contextId)).toEqual(["ctx-a", "ctx-b"]);
    expect(merged[0]?.preview).toBe("recent a");
  });
});

describe("writeEventConsoleRoute", () => {
  it("updates agent query params without navigation", () => {
    const original = window.location.href;
    writeEventConsoleRoute({
      agentPackage: "coordinator-agent",
      agentInstance: "default",
    });
    expect(window.location.search).toContain("agentPackage=coordinator-agent");
    expect(window.location.search).toContain("agentInstance=default");
    window.history.replaceState(null, "", original);
  });
});

describe("resolveAgentFromRoute", () => {
  const subscribed = agent("coordinator-agent", "default", [
    { schema_versions: ["host.source-records.v1"], source_kinds: ["clickup"] },
  ]);
  const noSubs = agent("extrospection-agent", "default", []);

  it("matches agents without subscriptions", () => {
    expect(
      resolveAgentFromRoute([subscribed, noSubs], "extrospection-agent", "default"),
    ).toBe(noSubs);
  });
});

describe("resolveSubscribedAgentFromRoute", () => {
  const subscribed = agent("coordinator-agent", "default", [
    { schema_versions: ["host.source-records.v1"], source_kinds: ["clickup"] },
  ]);
  const noSubs = agent("dispatch-echo", "default", []);

  it("matches package and instance when both are set", () => {
    expect(
      resolveSubscribedAgentFromRoute(
        [subscribed, noSubs],
        "coordinator-agent",
        "default",
      ),
    ).toBe(subscribed);
  });

  it("matches package when instance is omitted in the URL", () => {
    expect(
      resolveSubscribedAgentFromRoute([subscribed], "coordinator-agent", null),
    ).toBe(subscribed);
  });

  it("ignores agents without subscriptions", () => {
    expect(
      resolveSubscribedAgentFromRoute([noSubs], "dispatch-echo", "default"),
    ).toBeUndefined();
  });
});

const coordinatorAgent = {
  agentPackage: "coordinator-agent",
  agentInstanceId: "default",
};

describe("resolveObservationScope", () => {
  it("prefers draft continue-context over last dispatched scope for the same agent", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: {
        contextId: "ctx-dispatch",
        agentPackage: "coordinator-agent",
        agentInstanceId: "default",
      },
      draftScope: {
        kind: "existing_context",
        context_id: "ctx-draft",
      },
      selectedContextId: "ctx-history",
      previewProducedEvent: { context_id: "ctx-preview" },
      currentAgent: coordinatorAgent,
      mode: "compose",
    });
    expect(resolved?.contextId).toBe("ctx-draft");
  });

  it("ignores last dispatched scope from another agent and uses draft scope", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: {
        contextId: "ctx-notion",
        agentPackage: "support",
        agentInstanceId: "notion",
      },
      draftScope: {
        kind: "existing_context",
        context_id: "ctx-coordinator",
      },
      selectedContextId: "ctx-notion",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      mode: "compose",
    });
    expect(resolved?.contextId).toBe("ctx-coordinator");
  });

  it("falls back to preview when no dispatch or draft scope", () => {
    expect(
      scopeFromRecord({ context_id: "ctx-1", message_id: "msg-1" })?.contextId,
    ).toBe("ctx-1");
    const resolved = resolveObservationScope({
      lastPublishedScope: null,
      draftScope: { kind: "new_context" },
      selectedContextId: "ctx-history",
      previewProducedEvent: { context_id: "ctx-preview" },
      currentAgent: coordinatorAgent,
      mode: "compose",
    });
    expect(resolved?.contextId).toBe("ctx-preview");
  });

  it("does not use toolbar selected context in compose mode", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: null,
      draftScope: { kind: "new_context" },
      selectedContextId: "ctx-stale-picker",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      mode: "compose",
    });
    expect(resolved).toBeNull();
  });
});

describe("scopeFromDraftScope", () => {
  it("returns null for new_context", () => {
    expect(scopeFromDraftScope({ kind: "new_context" })).toBeNull();
  });

  it("includes task_id for existing_task", () => {
    expect(
      scopeFromDraftScope({
        kind: "existing_task",
        context_id: "ctx-1",
        task_id: "task-1",
      }),
    ).toEqual({ contextId: "ctx-1", taskId: "task-1" });
  });
});

describe("publishedScopeMatchesAgent", () => {
  it("matches when agent fields are absent (legacy)", () => {
    expect(
      publishedScopeMatchesAgent({ contextId: "ctx-1" }, coordinatorAgent),
    ).toBe(true);
  });

  it("rejects scope tagged for a different agent", () => {
    expect(
      publishedScopeMatchesAgent(
        {
          contextId: "ctx-1",
          agentPackage: "support",
          agentInstanceId: "notion",
        },
        coordinatorAgent,
      ),
    ).toBe(false);
  });
});

describe("resolveObservationScope history", () => {
  it("uses selected context when no dispatch scope", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: null,
      draftScope: { kind: "new_context" },
      selectedContextId: "ctx-history",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      mode: "history",
    });
    expect(resolved?.contextId).toBe("ctx-history");
    expect(scopeFromContextId("ctx-history")?.contextId).toBe("ctx-history");
  });
});

describe("buildOperatorPublishTraceMessages", () => {
  it("includes operator publish and outcome summary", () => {
    const rows = buildOperatorPublishTraceMessages({
      agentPackage: "pkg",
      agentInstanceId: "default",
      messageShape: undefined,
      envelope: {
        routingKey: "event:intake",
        messageType: "host.source-records.v1",
        sourceKind: "clickup",
        sourceKey: "clickup:list-1",
      },
      outcome: {
        subscribers_matched: 2,
        subscribers_accepted: 2,
        failures: [],
      },
      publishError: null,
    });
    expect(rows).toHaveLength(2);
    expect(rows[0]?.role).toBe("user");
    expect(rows[1]?.role).toBe("agent");
    expect(rows[1]?.text).toContain("Published 2/2");
  });
});

describe("resolveDispatchUnitTaskId", () => {
  it("picks dispatch-unit prefix from planning response", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        allTaskIds: ["dispatch-unit-abc", "other-task"],
      }),
    });
    vi.stubGlobal("fetch", fetchMock);
    const { resolveDispatchUnitTaskId } = await import("./dispatchObserve");
    await expect(resolveDispatchUnitTaskId("ctx-1")).resolves.toBe("dispatch-unit-abc");
    vi.unstubAllGlobals();
  });
});

describe("transcriptHasIngressUserRows", () => {
  it("detects ingress poll/unit user row ids", () => {
    expect(
      transcriptHasHostIngress([
        {
          id: "prov-user-ingress-poll-user:ctx:msg",
          role: "user",
          text: "1. Investigate publish ingress",
          timestamp: new Date(),
        },
      ]),
    ).toBe(true);
    expect(
      transcriptHasHostIngress([
        { id: "x", role: "user", text: "hello", timestamp: new Date() },
      ]),
    ).toBe(false);
  });
});

describe("event console scope", () => {
  it("requires task_id for existing_task scope", () => {
    const scope: EventDispatchScope = {
      kind: "existing_task",
      context_id: "ctx-1",
      task_id: "task-1",
    };
    const json = scopeToJson(scope);
    expect(json).toEqual({
      kind: "existing_task",
      context_id: "ctx-1",
      task_id: "task-1",
    });
  });

  it("encodes new_context without ids", () => {
    expect(scopeToJson({ kind: "new_context" })).toEqual({ kind: "new_context" });
  });
});

describe("validate request envelope", () => {
  it("uses message_type from derived envelope", () => {
    const shape: AgentDeliverableMessageShape = {
      message_shape_id: "system-callback-token",
      display_name: "System callback token",
      description: "",
      origin: "system/callback",
      payload_name: "Callback",
      wire_schema_version: "system.callback.v1",
      source_kind: "system/callback",
      payload_schema: {},
      samples: [
        {
          sample_id: "probe",
          label: "probe",
          source_key: "dispatch-echo:callback:probe",
          payload: {},
        },
      ],
      delivery_defaults: { routing_key: "system:callback" },
    };
    const envelope = deriveDispatchEnvelope(shape, shape.samples[0]);
    expect(envelope.messageType).toBe("system.callback.v1");
    expect(envelope.routingKey).toBe("system:callback");
  });
});
