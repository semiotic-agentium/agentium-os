import { describe, it, expect } from "vitest";
import { parseMermaidBlocks } from "../parseMermaid";

describe("parseMermaidBlocks", () => {
  it("extracts a single mermaid block", () => {
    const text = "Some text\n```mermaid\ngraph TD\n  A-->B\n```\nMore text";
    expect(parseMermaidBlocks(text)).toEqual(["graph TD\n  A-->B"]);
  });

  it("extracts multiple mermaid blocks", () => {
    const text = "```mermaid\nA\n```\nMiddle\n```mermaid\nB\n```";
    expect(parseMermaidBlocks(text)).toEqual(["A", "B"]);
  });

  it("returns empty array when no blocks", () => {
    expect(parseMermaidBlocks("No diagrams here")).toEqual([]);
  });

  it("ignores non-mermaid code blocks", () => {
    const text = "```javascript\nconsole.log('hi')\n```";
    expect(parseMermaidBlocks(text)).toEqual([]);
  });

  it("trims whitespace from extracted blocks", () => {
    const text = "```mermaid\n  graph TD  \n```";
    expect(parseMermaidBlocks(text)).toEqual(["graph TD"]);
  });

  it("handles Windows line endings", () => {
    const text = "```mermaid\r\ngraph TD\r\n  A-->B\r\n```";
    expect(parseMermaidBlocks(text)).toEqual(["graph TD\r\n  A-->B"]);
  });
});
