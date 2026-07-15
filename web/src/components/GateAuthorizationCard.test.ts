// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import GateAuthorizationCard from "./GateAuthorizationCard.vue";

const summary = {
  tier: 3,
  groundedIntent: "Archive inactive users in prod",
  postconditionCount: 2,
  rawPrompt: "Tier-3 authorization required.",
};

describe("GateAuthorizationCard", () => {
  it("renders grounded intent and postcondition count", () => {
    const wrapper = mount(GateAuthorizationCard, { props: { summary } });
    expect(wrapper.find("[data-testid='gate-authorization-card']").exists()).toBe(true);
    expect(wrapper.text()).toContain("Tier 3 authorization");
    expect(wrapper.text()).toContain("Archive inactive users in prod");
    expect(wrapper.text()).toContain("2 declared verification checks");
  });

  it("emits approve and deny", async () => {
    const wrapper = mount(GateAuthorizationCard, { props: { summary } });
    await wrapper.find("[data-testid='gate-authorization-approve']").trigger("click");
    await wrapper.find("[data-testid='gate-authorization-deny']").trigger("click");
    expect(wrapper.emitted("approve")).toHaveLength(1);
    expect(wrapper.emitted("deny")).toHaveLength(1);
  });

  it("disables actions when disabled prop is set", () => {
    const wrapper = mount(GateAuthorizationCard, {
      props: { summary, disabled: true },
    });
    const approve = wrapper.find("[data-testid='gate-authorization-approve']");
    const deny = wrapper.find("[data-testid='gate-authorization-deny']");
    expect((approve.element as HTMLButtonElement).disabled).toBe(true);
    expect((deny.element as HTMLButtonElement).disabled).toBe(true);
  });
});
