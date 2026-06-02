// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  fetchContextMermaidDiagram,
  invalidateContextMermaidSchedule,
  looksLikeMermaidDiagram,
  scheduleContextMermaidDiagram,
} from "./mermaidDiagram";

describe("looksLikeMermaidDiagram", () => {
  it("accepts sequenceDiagram with leading whitespace", () => {
    expect(looksLikeMermaidDiagram("  sequenceDiagram\n  A->>B: hi")).toBe(true);
  });

  it("rejects empty, HTML, JSON, and plain text", () => {
    expect(looksLikeMermaidDiagram("")).toBe(false);
    expect(looksLikeMermaidDiagram("<!DOCTYPE html>")).toBe(false);
    expect(looksLikeMermaidDiagram('{"error":"not found"}')).toBe(false);
    expect(looksLikeMermaidDiagram("graph TD\n  A --> B")).toBe(false);
  });
});

describe("fetchContextMermaidDiagram", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    invalidateContextMermaidSchedule();
  });

  it("returns diagram text when response is a sequence diagram", async () => {
    const body = "sequenceDiagram\n  Runner->>Agent: dispatch";
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: true, text: async () => body }),
    );
    await expect(fetchContextMermaidDiagram("ctx-1")).resolves.toBe(body);
  });

  it("returns empty string on non-ok, invalid body, or fetch error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: false, text: async () => "sequenceDiagram" }),
    );
    await expect(fetchContextMermaidDiagram("ctx-1")).resolves.toBe("");

    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: true, text: async () => "not mermaid" }),
    );
    await expect(fetchContextMermaidDiagram("ctx-1")).resolves.toBe("");

    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("network")));
    await expect(fetchContextMermaidDiagram("ctx-1")).resolves.toBe("");
  });

  it("dedupes concurrent fetches for the same context", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => "sequenceDiagram\n  A->>B",
    });
    vi.stubGlobal("fetch", fetchMock);

    const [a, b] = await Promise.all([
      fetchContextMermaidDiagram("ctx-dedupe"),
      fetchContextMermaidDiagram("ctx-dedupe"),
    ]);
    expect(a).toBe(b);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

describe("scheduleContextMermaidDiagram", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    invalidateContextMermaidSchedule();
  });

  it("coalesces rapid schedule calls into one fetch", async () => {
    vi.useFakeTimers();
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => "sequenceDiagram\n  A->>B",
    });
    vi.stubGlobal("fetch", fetchMock);

    const p1 = scheduleContextMermaidDiagram("ctx-sched");
    const p2 = scheduleContextMermaidDiagram("ctx-sched");
    await vi.advanceTimersByTimeAsync(400);

    const [r1, r2] = await Promise.all([p1, p2]);
    expect(r1).toBe(r2);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
