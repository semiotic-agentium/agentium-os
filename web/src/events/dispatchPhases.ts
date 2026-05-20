/** Event Console dispatch phase predicates (single source for UI + provenance pane). */

import type { EventDispatchPhase } from "../types/events";

/** All phases in operator-run order (documentation / exhaustiveness). */
export const EVENT_DISPATCH_PHASES = [
  "idle",
  "validating",
  "publishing",
  "recording",
  "live",
  "empty",
  "failed",
] as const satisfies readonly EventDispatchPhase[];

/** Phases where the operator action is still in progress (status pill + disabled controls). */
export const EVENT_DISPATCH_IN_FLIGHT_PHASES = [
  "validating",
  "publishing",
  "recording",
] as const;

export type EventDispatchInFlightPhase =
  (typeof EVENT_DISPATCH_IN_FLIGHT_PHASES)[number];

/** Phases where provenance should poll / show a live indicator on the trace pane. */
export const EVENT_DISPATCH_PROVENANCE_STREAMING_PHASES = [
  "publishing",
  "recording",
] as const;

export type EventDispatchProvenanceStreamingPhase =
  (typeof EVENT_DISPATCH_PROVENANCE_STREAMING_PHASES)[number];

function phaseIn<const T extends readonly string[]>(
  phase: EventDispatchPhase,
  set: T,
): phase is Extract<EventDispatchPhase, T[number]> {
  return (set as readonly string[]).includes(phase);
}

export function isEventDispatchInFlight(
  phase: EventDispatchPhase,
): phase is EventDispatchInFlightPhase {
  return phaseIn(phase, EVENT_DISPATCH_IN_FLIGHT_PHASES);
}

export function isEventDispatchProvenanceStreaming(
  phase: EventDispatchPhase,
): phase is EventDispatchProvenanceStreamingPhase {
  return phaseIn(phase, EVENT_DISPATCH_PROVENANCE_STREAMING_PHASES);
}
