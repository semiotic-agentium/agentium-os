import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import EventRunHeader from "./EventRunHeader.vue";

describe("EventRunHeader", () => {
  const baseProps = {
    histories: [],
    selectedContextId: null,
    historyLoading: false,
    historyFetchError: null,
    historyRunsHint: null,
  };

  it("renders New event CTA and run picker only", () => {
    const wrapper = mount(EventRunHeader, { props: baseProps });
    expect(wrapper.text()).toContain("New event");
    expect(wrapper.text()).toContain("Run");
    expect(wrapper.find(".run-pill").exists()).toBe(false);
    expect(wrapper.find(".context-chip").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("Open in Chat");
  });

  it("emits new-event when CTA clicked", async () => {
    const wrapper = mount(EventRunHeader, { props: baseProps });
    await wrapper.get(".run-header-cta").trigger("click");
    expect(wrapper.emitted("new-event")).toHaveLength(1);
  });
});
