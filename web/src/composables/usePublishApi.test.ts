// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { usePublishApi } from "./usePublishApi";

describe("usePublishApi", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(async (input: RequestInfo) => {
        const url = String(input);
        if (url.includes("/repository/publish")) {
          return {
            ok: true,
            json: async () => ({ hash: "abc123def456" }),
          } as Response;
        }
        if (url.includes("/deploy")) {
          return {
            ok: true,
            json: async () => ({ hash: "abc123def456", already_deployed: false }),
          } as Response;
        }
        if (url.includes("/deployments")) {
          return {
            ok: true,
            json: async () => [],
          } as Response;
        }
        return { ok: false, status: 404 } as Response;
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("loadAgent publishes then deploys when deployAfterPublish is true", async () => {
    const { loadAgent, phase, lastHash } = usePublishApi();
    const result = await loadAgent({
      name: "demo",
      rationale: "test",
      origin: "Original",
      source: { manifest: { name: "demo" }, ts_sources: [], baml_sources: [] },
    });

    expect(result?.hash).toBe("abc123def456");
    expect(lastHash.value).toBe("abc123def456");
    expect(phase.value).toBe("done");
    expect(vi.mocked(fetch).mock.calls.some((c) => String(c[0]).includes("/repository/publish"))).toBe(
      true,
    );
    expect(vi.mocked(fetch).mock.calls.some((c) => String(c[0]).includes("/deploy"))).toBe(true);
  });
});
