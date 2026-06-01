// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { deriveChatRunStatus, deriveEventRunStatus } from "./runStatus";

describe("deriveEventRunStatus", () => {
  it("returns executing when ack shows 0/1 units on live phase", () => {
    const status = deriveEventRunStatus({
      dispatchPhase: "live",
      hydrateState: "ready",
      publishError: null,
      waitingForIngress: false,
      contextId: "ctx-1",
      transcriptMessages: [],
      lastPublishOutcome: {
        subscribers_matched: 1,
        subscribers_accepted: 1,
        failures: [],
        acceptances: [
          {
            agent_package: "clickup-agent",
            agent_instance_id: "default",
            detail: "Processed ClickUp lifecycle ingress: 0/1 unit(s) from 1 record(s)",
          },
        ],
      },
    });
    expect(status.phase).toBe("executing");
    expect(status.label).toBe("Executing");
    expect(status.severity).toBe("progress");
    expect(status.active).toBe(true);
    expect(status.progress).toEqual({ done: 0, total: 1, noun: "units" });
  });

  it("returns complete when all units processed", () => {
    const status = deriveEventRunStatus({
      dispatchPhase: "live",
      hydrateState: "ready",
      publishError: null,
      waitingForIngress: false,
      contextId: "ctx-1",
      transcriptMessages: [],
      lastPublishOutcome: {
        subscribers_matched: 1,
        subscribers_accepted: 1,
        failures: [],
        acceptances: [
          {
            agent_package: "clickup-agent",
            agent_instance_id: "default",
            detail: "Processed ClickUp lifecycle ingress: 1/1 unit(s)",
          },
        ],
      },
    });
    expect(status.phase).toBe("complete");
    expect(status.severity).toBe("success");
    expect(status.active).toBe(false);
  });

  it("returns failed on subscriber rejection", () => {
    const status = deriveEventRunStatus({
      dispatchPhase: "failed",
      hydrateState: "ready",
      publishError: null,
      waitingForIngress: false,
      contextId: "ctx-1",
      transcriptMessages: [],
      lastPublishOutcome: {
        subscribers_matched: 1,
        subscribers_accepted: 0,
        failures: [{ agent_package: "a", agent_instance_id: "default", detail: "err" }],
      },
    });
    expect(status.phase).toBe("failed");
    expect(status.severity).toBe("error");
  });

  it("returns recording when waiting for ingress", () => {
    const status = deriveEventRunStatus({
      dispatchPhase: "empty",
      hydrateState: "waiting",
      publishError: null,
      waitingForIngress: true,
      contextId: "ctx-1",
      transcriptMessages: [],
      lastPublishOutcome: {
        subscribers_matched: 1,
        subscribers_accepted: 1,
        failures: [],
        acceptances: [
          {
            agent_package: "clickup-agent",
            agent_instance_id: "default",
            detail: "Processed ClickUp lifecycle ingress: 1/1 unit(s)",
          },
        ],
      },
    });
    expect(status.phase).toBe("recording");
    expect(status.label).toBe("Waiting for ingress");
    expect(status.active).toBe(true);
  });

  it("returns observing for deep-linked past runs with stale open tool rows", () => {
    const status = deriveEventRunStatus({
      dispatchPhase: "idle",
      hydrateState: "ready",
      publishError: null,
      waitingForIngress: false,
      contextId: "ctx-1779972010352-1",
      lastPublishOutcome: null,
      observeOnly: true,
      transcriptMessages: [
        {
          id: "msg-open",
          role: "agent",
          text: "",
          timestamp: new Date(0),
          contentBlocks: [
            {
              type: "tool",
              toolName: "support/clickup",
              status: "Running",
              events: [
                {
                  kind: "system_notice",
                  subtype: "Session step",
                  text: "open",
                },
              ],
            },
          ],
        },
      ],
    });
    expect(status.phase).toBe("idle");
    expect(status.label).toBe("Observing");
    expect(status.active).toBe(false);
  });
});

describe("deriveChatRunStatus", () => {
  it("returns waiting when input required", () => {
    const status = deriveChatRunStatus({
      isLoading: false,
      awaitingInput: true,
      hydrateState: "ready",
      workflowProgress: { phase: "idle", nodes: [], completedNodes: [] },
      messages: [],
      contextId: "ctx-1",
    });
    expect(status.phase).toBe("waiting");
    expect(status.label).toBe("Awaiting your reply");
  });

  it("returns executing when streaming", () => {
    const status = deriveChatRunStatus({
      isLoading: true,
      awaitingInput: false,
      hydrateState: "ready",
      workflowProgress: { phase: "idle", nodes: [], completedNodes: [] },
      messages: [],
      contextId: "ctx-1",
    });
    expect(status.phase).toBe("executing");
    expect(status.active).toBe(true);
  });

  it("returns coordinator workflow steps", () => {
    const status = deriveChatRunStatus({
      isLoading: true,
      awaitingInput: false,
      hydrateState: "ready",
      workflowProgress: {
        phase: "execution",
        pipelineActive: true,
        nodes: [{ name: "fetch", status: "running" }],
        completedNodes: [],
      },
      messages: [],
      contextId: "ctx-1",
    });
    expect(status.phase).toBe("executing");
    expect(status.steps?.length).toBe(4);
    expect(status.steps?.find((s) => s.key === "execution")?.state).toBe("active");
    expect(status.detail).toContain("fetch");
  });
});
