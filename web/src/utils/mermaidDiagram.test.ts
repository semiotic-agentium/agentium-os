import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchContextMermaidDiagram, looksLikeMermaidDiagram } from "./mermaidDiagram";

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
});
