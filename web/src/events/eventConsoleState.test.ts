import { describe, expect, it } from "vitest";
import type { EventObservationState } from "../types/events";
import {
  buildObservationScopeResolveInput,
  changeDraftScopeKind,
  createInitialObservation,
  isEventDispatchScopeKind,
  resolveObservedScopeIds,
  scopeContextIdFromDraft,
  shouldOfferApplyObservedScope,
} from "./eventConsoleState";

describe("eventConsoleState", () => {
  it("isEventDispatchScopeKind narrows scope kinds", () => {
    expect(isEventDispatchScopeKind("new_context")).toBe(true);
    expect(isEventDispatchScopeKind("bogus")).toBe(false);
  });

  it("changeDraftScopeKind preserves context_id when switching to existing_context", () => {
    const next = changeDraftScopeKind(
      { kind: "existing_task", context_id: "ctx-1", task_id: "task-1" },
      "existing_context",
    );
    expect(next).toEqual({ kind: "existing_context", context_id: "ctx-1" });
  });

  it("scopeContextIdFromDraft returns empty for new_context", () => {
    expect(scopeContextIdFromDraft({ kind: "new_context" })).toBe("");
  });

  it("shouldOfferApplyObservedScope only for new_context draft with observed id", () => {
    expect(
      shouldOfferApplyObservedScope("ctx-1", { kind: "new_context" }),
    ).toBe(true);
    expect(
      shouldOfferApplyObservedScope("ctx-1", {
        kind: "existing_context",
        context_id: "ctx-1",
      }),
    ).toBe(false);
  });

  it("resolveObservedScopeIds uses picker observation over publish scope", () => {
    const observation: EventObservationState = {
      contextId: "ctx-picker",
      source: "picker",
    };
    const input = buildObservationScopeResolveInput({
      lastPublishedScope: {
        contextId: "ctx-publish",
        agentPackage: "coordinator-agent",
        agentInstanceId: "default",
      },
      draft: {
        agent_package: "coordinator-agent",
        agent_instance_id: "default",
        messages: [],
        scope: { kind: "new_context" },
        message_id: "",
        metadata: {},
      },
      observation,
      validation: null,
    });
    expect(resolveObservedScopeIds(input).contextId).toBe("ctx-picker");
  });

  it("createInitialObservation starts in draft mode", () => {
    expect(createInitialObservation()).toEqual({
      contextId: null,
      source: "draft",
    });
  });

  it("resolveObservedScopeIds prefers observation.taskId from dispatch-unit resolution", () => {
    const input = buildObservationScopeResolveInput({
      lastPublishedScope: null,
      draft: {
        agent_package: "clickup-agent",
        agent_instance_id: "default",
        messages: [],
        scope: { kind: "new_context" },
        message_id: "",
        metadata: {},
      },
      observation: {
        contextId: "ctx-1",
        source: "picker",
        taskId: "dispatch-unit-abc",
      },
      validation: null,
    });
    expect(resolveObservedScopeIds(input).taskId).toBe("dispatch-unit-abc");
  });
});
