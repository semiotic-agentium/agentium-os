import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import EventRunStatusStrip from "./EventRunStatusStrip.vue";

describe("EventRunStatusStrip", () => {
  it("shows failure strip with subscriber details", () => {
    const wrapper = mount(EventRunStatusStrip, {
      props: {
        dispatchPhase: "failed",
        hydrateState: "ready",
        contextId: "ctx-1",
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
    expect(wrapper.find(".event-run-status-strip--error").exists()).toBe(true);
    expect(wrapper.text()).toContain("0 of 1");
    expect(wrapper.text()).toContain("CLICKUP_API_KEY");
  });

  it("shows waiting for ingress message", () => {
    const wrapper = mount(EventRunStatusStrip, {
      props: {
        dispatchPhase: "empty",
        hydrateState: "waiting",
        contextId: "ctx-1",
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

  it("stays mounted on live without auto-dismiss", () => {
    const wrapper = mount(EventRunStatusStrip, {
      props: {
        dispatchPhase: "live",
        hydrateState: "ready",
        contextId: "ctx-live",
        publishError: null,
        waitingForIngress: false,
        lastPublishOutcome: {
          subscribers_matched: 1,
          subscribers_accepted: 1,
          failures: [],
        },
      },
    });
    expect(wrapper.find(".event-run-status-strip").exists()).toBe(true);
    expect(wrapper.text()).toContain("Live");
  });
});
