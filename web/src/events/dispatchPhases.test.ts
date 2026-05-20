import { describe, expect, it } from "vitest";
import type { EventDispatchPhase } from "../types/events";
import {
  EVENT_DISPATCH_PHASES,
  isEventDispatchInFlight,
  isEventDispatchProvenanceStreaming,
} from "./dispatchPhases";

describe("dispatchPhases", () => {
  it("lists every EventDispatchPhase exactly once", () => {
    const phases: EventDispatchPhase[] = [
      "idle",
      "validating",
      "publishing",
      "recording",
      "live",
      "empty",
      "failed",
    ];
    expect([...EVENT_DISPATCH_PHASES]).toEqual(phases);
  });

  it("classifies in-flight vs provenance streaming", () => {
    expect(isEventDispatchInFlight("validating")).toBe(true);
    expect(isEventDispatchInFlight("publishing")).toBe(true);
    expect(isEventDispatchInFlight("recording")).toBe(true);
    expect(isEventDispatchInFlight("idle")).toBe(false);
    expect(isEventDispatchInFlight("live")).toBe(false);

    expect(isEventDispatchProvenanceStreaming("publishing")).toBe(true);
    expect(isEventDispatchProvenanceStreaming("recording")).toBe(true);
    expect(isEventDispatchProvenanceStreaming("validating")).toBe(false);
    expect(isEventDispatchProvenanceStreaming("dispatching" as EventDispatchPhase)).toBe(
      false,
    );
  });
});
