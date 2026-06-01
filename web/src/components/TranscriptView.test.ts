// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import TranscriptView from "./TranscriptView.vue";
import type { EventRunMeta } from "../events/eventTranscriptModel";

function idleEventMeta(overrides: Partial<EventRunMeta> = {}): EventRunMeta {
  return {
    dispatchPhase: "empty",
    hydrateState: "idle",
    lastPublishOutcome: null,
    publishError: null,
    waitingForIngress: false,
    hasPublishedRun: false,
    ...overrides,
  };
}

describe("TranscriptView event variant", () => {
  it("renders onboarding panel when no context and idle", () => {
    const wrapper = mount(TranscriptView, {
      props: {
        messages: [],
        variant: "event",
        hydrateState: "idle",
        selectedContextId: null,
        eventRunMeta: idleEventMeta(),
      },
    });
    expect(wrapper.text()).toContain("Observe an Event Run");
    expect(wrapper.text()).toContain("Choose agent and message type");
    expect(wrapper.find(".empty-state-action--primary").exists()).toBe(true);
  });

  it("emits compose-event from onboarding primary action", async () => {
    const wrapper = mount(TranscriptView, {
      props: {
        messages: [],
        variant: "event",
        hydrateState: "idle",
        selectedContextId: null,
        eventRunMeta: idleEventMeta(),
      },
    });
    await wrapper.get(".empty-state-action--primary").trigger("click");
    expect(wrapper.emitted("compose-event")).toHaveLength(1);
  });

  it("emits focus-event-run from secondary action", async () => {
    const wrapper = mount(TranscriptView, {
      props: {
        messages: [],
        variant: "event",
        hydrateState: "idle",
        selectedContextId: null,
        eventRunMeta: idleEventMeta(),
      },
    });
    const buttons = wrapper.findAll(".empty-state-action");
    const pickRun = buttons.find((b) => b.text().includes("Pick a run"));
    expect(pickRun).toBeDefined();
    await pickRun!.trigger("click");
    expect(wrapper.emitted("focus-event-run")).toHaveLength(1);
  });

  it("shows skeleton timeline rows while provenance loads after publish", () => {
    const wrapper = mount(TranscriptView, {
      props: {
        messages: [],
        variant: "event",
        hydrateState: "loading",
        selectedContextId: "ctx-1",
        hasPublishedRun: true,
        eventRunMeta: idleEventMeta({
          hydrateState: "loading",
          hasPublishedRun: true,
          waitingForIngress: true,
        }),
      },
    });
    expect(wrapper.findAll(".event-lane-card--skeleton").length).toBeGreaterThan(0);
  });
});
