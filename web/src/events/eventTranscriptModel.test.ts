import { describe, expect, it } from "vitest";
import type { ChatMessage } from "../types/a2a";
import {
  EVENT_TRANSCRIPT_INGRESS_KEY,
  mergeMessagesByRowKey,
  normalizeEventTranscriptRows,
  transcriptPhaseLabel,
  type EventRunMeta,
} from "./eventTranscriptModel";
import { buildIngressWireUserMessage } from "./dispatchObserve";

function baseMeta(overrides: Partial<EventRunMeta> = {}): EventRunMeta {
  return {
    dispatchPhase: "live",
    hydrateState: "ready",
    lastPublishOutcome: null,
    publishError: null,
    waitingForIngress: false,
    hasPublishedRun: false,
    ...overrides,
  };
}

describe("transcriptPhaseLabel", () => {
  it("maps dispatch and hydrate states to operator labels", () => {
    expect(transcriptPhaseLabel("validating", "idle")).toBe("Validating draft…");
    expect(transcriptPhaseLabel("publishing", "idle")).toBe("Publishing to subscribers…");
    expect(transcriptPhaseLabel("recording", "loading")).toBe("Recording provenance…");
    expect(transcriptPhaseLabel("live", "ready")).toBe("Live");
    expect(transcriptPhaseLabel("failed", "ready")).toBe("Publish failed");
  });
});

describe("normalizeEventTranscriptRows", () => {
  it("omits success publish milestone when status strip owns acceptance summary", () => {
    const rows = normalizeEventTranscriptRows(
      [],
      baseMeta({
        lastPublishOutcome: {
          subscribers_matched: 1,
          subscribers_accepted: 1,
          failures: [],
        },
        hasPublishedRun: true,
      }),
    );
    expect(rows.some((r) => r.kind === "milestone")).toBe(false);
  });

  it("keeps error publish milestone until provenance operational rows land", () => {
    const rows = normalizeEventTranscriptRows(
      [],
      baseMeta({
        lastPublishOutcome: {
          subscribers_matched: 1,
          subscribers_accepted: 0,
          failures: [{ agent_package: "a", agent_instance_id: "default", detail: "rejected" }],
        },
      }),
    );
    expect(rows.some((r) => r.kind === "milestone")).toBe(true);
  });

  it("skips operator publish trace chat bubbles", () => {
    const rows = normalizeEventTranscriptRows(
      [
        {
          id: "event-console-local-operator-publish-trace-outcome",
          role: "agent",
          text: "Published 2/2",
          timestamp: new Date(),
        },
      ],
      baseMeta({
        lastPublishOutcome: {
          subscribers_matched: 2,
          subscribers_accepted: 2,
          failures: [],
        },
      }),
    );
    expect(rows.some((r) => r.kind === "agent_turn")).toBe(false);
  });

  it("adds skeleton rows while waiting for hydrate", () => {
    const rows = normalizeEventTranscriptRows([], baseMeta({
      hasPublishedRun: true,
      hydrateState: "loading",
      waitingForIngress: true,
    }), { includeSkeletons: true });
    expect(rows.filter((r) => r.kind === "skeleton")).toHaveLength(3);
  });

  it("maps ingress wire to stable key", () => {
    const local = buildIngressWireUserMessage([{ k: 1 }]);
    const rows = normalizeEventTranscriptRows([local], baseMeta(), { includeSkeletons: false });
    const ingress = rows.find((r) => r.kind === "ingress_wire");
    expect(ingress?.key).toBe(EVENT_TRANSCRIPT_INGRESS_KEY);
    expect(ingress && ingress.kind === "ingress_wire" && ingress.pending).toBe(false);
  });
});

describe("mergeMessagesByRowKey", () => {
  it("replaces local ingress in place when provenance lands without changing count", () => {
    const local = [buildIngressWireUserMessage([{ x: 1 }])];
    const provenance: ChatMessage[] = [
      {
        id: "prov-user-ingress-unit-user:ctx:unit",
        role: "user",
        speakerKind: "ingress",
        text: "host line",
        timestamp: new Date(),
      },
    ];
    const merged = mergeMessagesByRowKey(provenance, local);
    expect(merged).toHaveLength(1);
    expect(merged[0]?.id).toContain("ingress-unit-user");
  });
});
