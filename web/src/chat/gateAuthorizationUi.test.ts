// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  GATE_DENY_MESSAGE,
  isGateAuthorizationMetadata,
  isGateAuthorizationPrompt,
  parseGateAuthorizationPrompt,
} from "./gateAuthorizationUi";

describe("parseGateAuthorizationPrompt", () => {
  it("parses structured tier-3 host prompt", () => {
    const summary = parseGateAuthorizationPrompt(
      "Tier-3 authorization required.\n\nGrounded intent: Archive inactive users\nPostconditions declared: 2\n\nReply to approve.",
    );
    expect(summary).toMatchObject({
      tier: 3,
      groundedIntent: "Archive inactive users",
      postconditionCount: 2,
    });
  });

  it("detects gate authorization metadata", () => {
    expect(isGateAuthorizationMetadata({ gateAuthorization: true })).toBe(true);
    expect(isGateAuthorizationMetadata({})).toBe(false);
  });

  it("uses stable deny message prefix", () => {
    expect(GATE_DENY_MESSAGE.startsWith("[gate-deny]")).toBe(true);
  });

  it("detects gate authorization prompt prefix", () => {
    expect(isGateAuthorizationPrompt("Tier-3 authorization required.\n\nGrounded intent: x")).toBe(
      true,
    );
    expect(isGateAuthorizationPrompt("Please clarify the target path")).toBe(false);
  });
});
