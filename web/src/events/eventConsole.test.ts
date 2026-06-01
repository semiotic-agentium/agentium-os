// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
  buildEventConsoleLocalTranscript,
  buildIngressWireUserMessage,
  buildOperatorPublishTraceMessages,
  mergeEventConsoleTranscript,
  publishedScopeMatchesAgent,
  resolveObservationScope,
  scopeFromContextId,
  scopeFromDraftScope,
  scopeFromRecord,
  transcriptHasIngressUserRows,
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
  it("updates event-scoped query params without navigation", () => {
    const original = window.location.href;
    writeEventConsoleRoute({
      agentPackage: "coordinator-agent",
      agentInstance: "default",
    });
    const params = new URLSearchParams(window.location.search);
    expect(params.get("view")).toBe("events");
    expect(params.get("eventAgentPackage")).toBe("coordinator-agent");
    expect(params.get("eventAgentInstance")).toBe("default");
    expect(params.get("agentPackage")).toBeNull();
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
      observedContextId: "ctx-history",
      previewProducedEvent: { context_id: "ctx-preview" },
      currentAgent: coordinatorAgent,
      observationSource: "draft",
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
      observedContextId: "ctx-notion",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      observationSource: "draft",
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
      observedContextId: "ctx-history",
      previewProducedEvent: { context_id: "ctx-preview" },
      currentAgent: coordinatorAgent,
      observationSource: "draft",
    });
    expect(resolved?.contextId).toBe("ctx-preview");
  });

  it("does not use toolbar selected context in draft mode", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: null,
      draftScope: { kind: "new_context" },
      observedContextId: "ctx-stale-picker",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      observationSource: "draft",
    });
    expect(resolved).toBeNull();
  });

  it("picker wins over publish and new_context draft", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: {
        contextId: "ctx-dispatch",
        agentPackage: "coordinator-agent",
        agentInstanceId: "default",
      },
      draftScope: { kind: "new_context" },
      observedContextId: "ctx-history",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      observationSource: "picker",
    });
    expect(resolved?.contextId).toBe("ctx-history");
  });

  it("publish source prefers last publish scope", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: {
        contextId: "ctx-dispatch",
        agentPackage: "coordinator-agent",
        agentInstanceId: "default",
      },
      draftScope: { kind: "new_context" },
      observedContextId: "ctx-other",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      observationSource: "publish",
    });
    expect(resolved?.contextId).toBe("ctx-dispatch");
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

describe("resolveObservationScope picker", () => {
  it("uses selected context when observation source is picker", () => {
    const resolved = resolveObservationScope({
      lastPublishedScope: null,
      draftScope: { kind: "new_context" },
      observedContextId: "ctx-history",
      previewProducedEvent: null,
      currentAgent: coordinatorAgent,
      observationSource: "picker",
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

describe("buildEventConsoleLocalTranscript", () => {
  it("includes only optimistic ingress wire preview", () => {
    const rows = buildEventConsoleLocalTranscript({
      previewProducedEvent: {
        context_id: "ctx-1",
        messages: [{ records: [{ record_kind: "clickup.lifecycle_event", key: "k" }] }],
      },
      outcome: {
        subscribers_matched: 1,
        subscribers_accepted: 1,
        failures: [],
      },
      publishError: null,
      agentPackage: "clickup-agent",
      agentInstanceId: "default",
      messageShape: undefined,
      envelope: null,
    });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.speakerKind).toBe("ingress");
    expect(rows.some((m) => m.role === "agent" && m.text?.includes("Published"))).toBe(false);
  });
});

describe("mergeEventConsoleTranscript", () => {
  it("replaces local ingress in place when provenance lands", () => {
    const local = [buildIngressWireUserMessage([{ x: 1 }])];
    const provenance = [
      {
        id: "prov-user-ingress-unit-user:ctx:unit",
        role: "user" as const,
        text: "host line",
        timestamp: new Date(),
      },
    ];
    const merged = mergeEventConsoleTranscript(provenance, local);
    expect(merged).toHaveLength(1);
    expect(merged[0]?.id).toContain("ingress-unit-user");
  });

  it("drops local publish trace rows when provenance operational failures exist", () => {
    const local = [
      {
        id: "event-console-local-operator-publish-trace-outcome",
        role: "agent" as const,
        text: "Published 1/2 subscriber(s)\nFailures:\n- coord/default: rejected detail",
        timestamp: new Date(),
      },
    ];
    const provenanceWithOperational = [
      {
        id: "prov-user-ingress-unit-user:ctx:unit",
        role: "user" as const,
        speakerKind: "ingress" as const,
        text: "wire",
        timestamp: new Date(),
      },
      {
        id: "prov-op-1",
        role: "agent" as const,
        speakerKind: "host" as const,
        text: "",
        timestamp: new Date(),
        contentBlocks: [
          {
            type: "operational" as const,
            kind: "dispatch_rejected",
            severity: "error",
            summary: "Host dispatch rejected",
            detail: "rejected detail",
            agentPackage: "coord",
            agentInstanceId: "default",
          },
        ],
      },
    ];
    const merged = mergeEventConsoleTranscript(provenanceWithOperational, local);
    expect(merged.some((m) => m.id.includes("publish-trace-outcome"))).toBe(false);
    expect(merged.some((m) => m.id.includes("ingress-unit-user"))).toBe(true);
  });
});

describe("transcriptHasIngressUserRows", () => {
  it("detects ingress poll/unit user row ids", () => {
    expect(
      transcriptHasIngressUserRows([
        {
          id: "prov-user-ingress-poll-user:ctx:msg",
          role: "user",
          text: "1. Investigate publish ingress",
          timestamp: new Date(),
        },
      ]),
    ).toBe(true);
    expect(
      transcriptHasIngressUserRows([
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
