import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import TranscriptView from "./TranscriptView.vue";

describe("TranscriptView event variant", () => {
  it("renders onboarding panel when no context and idle", () => {
    const wrapper = mount(TranscriptView, {
      props: {
        messages: [],
        variant: "event",
        hydrateState: "idle",
        selectedContextId: null,
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
      },
    });
    const buttons = wrapper.findAll(".empty-state-action");
    const pickRun = buttons.find((b) => b.text().includes("Pick a run"));
    expect(pickRun).toBeDefined();
    await pickRun!.trigger("click");
    expect(wrapper.emitted("focus-event-run")).toHaveLength(1);
  });
});
