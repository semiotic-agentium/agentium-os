import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import RunStatusIndicator from "./RunStatusIndicator.vue";

describe("RunStatusIndicator", () => {
  it("maps severity to dot styling", () => {
    const wrapper = mount(RunStatusIndicator, {
      props: {
        variant: "compact",
        status: {
          phase: "executing",
          label: "Executing",
          severity: "progress",
          active: true,
        },
      },
    });
    expect(wrapper.find(".run-status--progress").exists()).toBe(true);
    expect(wrapper.text()).toContain("Executing");
  });

  it("renders workflow steps in banner variant", () => {
    const wrapper = mount(RunStatusIndicator, {
      props: {
        variant: "banner",
        status: {
          phase: "executing",
          label: "Execution",
          severity: "progress",
          active: true,
          steps: [
            { key: "discovery", label: "Discovery", state: "done" },
            { key: "execution", label: "Execution", state: "active" },
          ],
        },
      },
    });
    expect(wrapper.find(".run-status__steps").exists()).toBe(true);
    expect(wrapper.text()).toContain("Discovery");
    expect(wrapper.text()).toContain("Execution");
  });
});
