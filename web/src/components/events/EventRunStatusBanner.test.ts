import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import EventRunStatusBanner from "./EventRunStatusBanner.vue";

describe("EventRunStatusBanner", () => {
  it("shows failure banner with subscriber details", () => {
    const wrapper = mount(EventRunStatusBanner, {
      props: {
        dispatchPhase: "failed",
        publishError: null,
        waitingForIngress: false,
        lastPublishOutcome: {
          subscribers_matched: 1,
          subscribers_accepted: 0,
          failures: [
            {
              agent_package: "clickup-agent",
              agent_instance_id: "default",
              detail: "CLICKUP_API_KEY not resolved",
            },
          ],
        },
      },
    });
    expect(wrapper.find(".status-banner--error").exists()).toBe(true);
    expect(wrapper.text()).toContain("0 of 1");
    expect(wrapper.text()).toContain("CLICKUP_API_KEY");
  });

  it("shows waiting for ingress banner", () => {
    const wrapper = mount(EventRunStatusBanner, {
      props: {
        dispatchPhase: "empty",
        publishError: null,
        waitingForIngress: true,
        lastPublishOutcome: {
          subscribers_matched: 1,
          subscribers_accepted: 1,
          acceptances: [
            {
              agent_package: "clickup-agent",
              agent_instance_id: "default",
              detail: "Processed ClickUp lifecycle ingress: 1/1 unit(s)",
            },
          ],
          failures: [],
        },
      },
    });
    expect(wrapper.text()).toContain("Waiting for host ingress");
  });
});
