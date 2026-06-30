// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { InstanceHttpError, normalizeInstanceUrl, readInstanceErrorBody } from "./instanceApi";

describe("normalizeInstanceUrl", () => {
  it("adds http scheme and strips trailing slash", () => {
    expect(normalizeInstanceUrl("127.0.0.1:18080/")).toBe("http://127.0.0.1:18080");
  });

  it("preserves explicit https", () => {
    expect(normalizeInstanceUrl("https://agentium.example.com")).toBe(
      "https://agentium.example.com",
    );
  });
});

describe("InstanceHttpError", () => {
  it("clips error bodies", async () => {
    const response = new Response("x".repeat(100), { status: 502, statusText: "Bad Gateway" });
    const body = await readInstanceErrorBody(response, 20);
    const err = new InstanceHttpError(502, body, 20);
    expect(err.status).toBe(502);
    expect(err.body.length).toBeLessThanOrEqual(20);
    expect(err.message).toContain("HTTP 502");
  });
});
