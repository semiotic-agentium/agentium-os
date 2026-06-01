// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import EventRunStatusStrip from "./EventRunStatusStrip.vue";
import type { OperatorRunStatus } from "../../operator/runStatus";

describe("EventRunStatusStrip", () => {
  const executingStatus: OperatorRunStatus = {
    phase: "executing",
    label: "Executing",
    detail: "0/1 units · clickup-agent/default",
    severity: "progress",
    active: true,
    progress: { done: 0, total: 1, noun: "units" },
  };

  it("shows failure strip with subscriber details", () => {
    const wrapper = mount(EventRunStatusStrip, {
      props: {
        status: {
          phase: "failed",
          label: "Publish failed",
          detail: "0 of 1 subscriber(s) accepted",
          severity: "error",
          active: false,
        },
        contextId: "ctx-1",
        publishError: null,
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
    expect(wrapper.text()).toContain("Publish failed");
    expect(wrapper.text()).toContain("CLICKUP_API_KEY");
  });

  it("shows executing status for incomplete units", () => {
    const wrapper = mount(EventRunStatusStrip, {
      props: {
        status: executingStatus,
        contextId: "ctx-1",
        publishError: null,
        lastPublishOutcome: {
          subscribers_matched: 1,
          subscribers_accepted: 1,
          failures: [],
          acceptances: [
            {
              agent_package: "clickup-agent",
              agent_instance_id: "default",
              detail: "Processed ClickUp lifecycle ingress: 0/1 unit(s)",
            },
          ],
        },
      },
    });
    expect(wrapper.text()).toContain("Executing");
    expect(wrapper.text()).not.toContain("Live");
    expect(wrapper.find(".run-status--progress").exists()).toBe(true);
  });

  it("stays mounted on complete without auto-dismiss", () => {
    const wrapper = mount(EventRunStatusStrip, {
      props: {
        status: {
          phase: "complete",
          label: "Complete",
          severity: "success",
          active: false,
        },
        contextId: "ctx-live",
        publishError: null,
        lastPublishOutcome: {
          subscribers_matched: 1,
          subscribers_accepted: 1,
          failures: [],
        },
      },
    });
    expect(wrapper.find(".run-status").exists()).toBe(true);
    expect(wrapper.text()).toContain("Complete");
  });
});
