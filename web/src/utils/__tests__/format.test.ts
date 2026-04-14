import { describe, it, expect } from "vitest";
import {
  formatCompact,
  formatDuration,
  shortId,
  normalizeGroupValue,
  asDisplayIdentity,
  groupValueAt,
} from "../format";

describe("formatCompact", () => {
  it("formats millions", () => {
    expect(formatCompact(1_500_000)).toBe("1.5M");
  });
  it("formats thousands", () => {
    expect(formatCompact(2_300)).toBe("2.3k");
  });
  it("passes through small numbers", () => {
    expect(formatCompact(42)).toBe("42");
  });
  it("formats exactly 1M", () => {
    expect(formatCompact(1_000_000)).toBe("1.0M");
  });
  it("formats exactly 1k", () => {
    expect(formatCompact(1_000)).toBe("1.0k");
  });
});

describe("formatDuration", () => {
  it("formats seconds", () => {
    expect(formatDuration(1500)).toBe("1.50s");
  });
  it("formats milliseconds", () => {
    expect(formatDuration(42)).toBe("42ms");
  });
  it("formats exactly 1s", () => {
    expect(formatDuration(1000)).toBe("1.00s");
  });
  it("handles zero", () => {
    expect(formatDuration(0)).toBe("0ms");
  });
});

describe("shortId", () => {
  it("truncates long IDs", () => {
    expect(shortId("abcdef01-2345-6789-abcd")).toBe("abcdef01...abcd");
  });
  it("passes through short IDs", () => {
    expect(shortId("abc123")).toBe("abc123");
  });
  it("passes through exactly 12 chars", () => {
    expect(shortId("abcdefghijkl")).toBe("abcdefghijkl");
  });
});

describe("normalizeGroupValue", () => {
  it("returns trimmed value", () => {
    expect(normalizeGroupValue("  hello  ")).toBe("hello");
  });
  it("returns undefined for empty string", () => {
    expect(normalizeGroupValue("")).toBeUndefined();
  });
  it("returns undefined for whitespace-only", () => {
    expect(normalizeGroupValue("   ")).toBeUndefined();
  });
  it("returns undefined for null", () => {
    expect(normalizeGroupValue(null)).toBeUndefined();
  });
  it("returns undefined for undefined", () => {
    expect(normalizeGroupValue(undefined)).toBeUndefined();
  });
});

describe("asDisplayIdentity", () => {
  it("returns package/version when both present", () => {
    expect(asDisplayIdentity("id-1", "my-agent", "1.0.0")).toBe("my-agent/1.0.0");
  });
  it("returns package when only package present", () => {
    expect(asDisplayIdentity("id-1", "my-agent")).toBe("my-agent");
  });
  it("returns shortened agent ID as fallback", () => {
    expect(asDisplayIdentity("abcdef01-2345-6789-abcd")).toBe("abcdef01...abcd");
  });
  it("returns 'unknown-agent' when all unknown", () => {
    expect(asDisplayIdentity("unknown", "unknown", "unknown")).toBe("unknown-agent");
  });
  it("returns 'unknown-agent' when all undefined", () => {
    expect(asDisplayIdentity()).toBe("unknown-agent");
  });
});

describe("groupValueAt", () => {
  it("extracts from values array", () => {
    expect(groupValueAt(["a", "b", "c"], "x|y|z", 1)).toBe("b");
  });
  it("falls back to pipe-separated groupKey", () => {
    expect(groupValueAt(undefined, "x|y|z", 2)).toBe("z");
  });
  it("falls back when array value is null", () => {
    expect(groupValueAt([null, null, "c"], "x|y|z", 0)).toBe("x");
  });
  it("returns undefined for out-of-bounds", () => {
    expect(groupValueAt(["a"], "x", 5)).toBeUndefined();
  });
});
