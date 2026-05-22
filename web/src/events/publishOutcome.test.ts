import { describe, expect, it } from "vitest";
import {
  formatPublishAcceptanceSummary,
  isNoopSubscriberDetail,
  publishHadNoEffectiveWork,
} from "./publishOutcome";

describe("publishOutcome", () => {
  it("detects noop subscriber details", () => {
    expect(isNoopSubscriberDetail("No lifecycle records in batch.")).toBe(true);
    expect(isNoopSubscriberDetail("Processed ClickUp lifecycle ingress: 1/1")).toBe(false);
  });

  it("flags zero subscribers as no effective work", () => {
    expect(
      publishHadNoEffectiveWork({
        subscribers_matched: 0,
        subscribers_accepted: 0,
        failures: [],
      }),
    ).toBe(true);
  });

  it("formats acceptance lines", () => {
    const text = formatPublishAcceptanceSummary({
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
    });
    expect(text).toContain("clickup-agent/default");
    expect(text).toContain("Processed");
  });
});
