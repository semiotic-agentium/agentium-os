// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  incidentSeverityClass,
  postureChipClass,
  postureLabel,
  preventionRatioLabel,
} from "./semioticPolicyUi";

describe("semioticPolicyUi", () => {
  it("maps posture labels", () => {
    expect(postureLabel("off")).toBe("Off");
    expect(postureLabel("audit")).toBe("Audit");
    expect(postureLabel("enforce")).toBe("Enforce");
  });

  it("maps posture chip classes", () => {
    expect(postureChipClass("enforce")).toContain("enforce");
  });

  it("maps incident severity classes", () => {
    expect(incidentSeverityClass("critical")).toContain("critical");
    expect(incidentSeverityClass("warning")).toContain("warning");
  });

  it("formats prevention ratio", () => {
    expect(preventionRatioLabel(0.75)).toBe("75%");
    expect(preventionRatioLabel(null)).toBe("—");
  });
});
