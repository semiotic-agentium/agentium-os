import { describe, expect, it, vi } from "vitest";
import {
  bumpTraceRefreshOnHistoryIngress,
  useTraceRefreshGeneration,
} from "./useTraceRefreshGeneration";

describe("useTraceRefreshGeneration", () => {
  it("bumps generation and calls onBump when active", () => {
    const onBump = vi.fn();
    const trace = useTraceRefreshGeneration({
      when: () => true,
      onBump,
    });
    trace.bumpTraceRefresh();
    expect(trace.traceRefreshGeneration.value).toBe(1);
    expect(onBump).toHaveBeenCalledTimes(1);
  });

  it("skips bump when when() is false unless forced", () => {
    const onBump = vi.fn();
    const trace = useTraceRefreshGeneration({
      when: () => false,
      onBump,
    });
    trace.bumpTraceRefresh();
    expect(trace.traceRefreshGeneration.value).toBe(0);
    trace.bumpTraceRefresh(true);
    expect(trace.traceRefreshGeneration.value).toBe(1);
    expect(onBump).toHaveBeenCalledTimes(1);
  });

  it("dedupes snapshot by version when transcript is loaded", () => {
    const trace = useTraceRefreshGeneration();
    trace.setHistoryVersion("v1");
    expect(trace.isRedundantSnapshot("v1", true)).toBe(true);
    expect(trace.isRedundantSnapshot("v1", false)).toBe(false);
    expect(trace.isRedundantSnapshot("v2", true)).toBe(false);
  });

  it("bumps only on advancing history version", () => {
    const trace = useTraceRefreshGeneration();
    trace.bumpOnHistoryVersion("v1");
    expect(trace.traceRefreshGeneration.value).toBe(1);
    trace.bumpOnHistoryVersion("v1");
    expect(trace.traceRefreshGeneration.value).toBe(1);
    trace.bumpOnHistoryVersion("v2");
    expect(trace.traceRefreshGeneration.value).toBe(2);
  });
});

describe("bumpTraceRefreshOnHistoryIngress", () => {
  it("bumps on applied ingress effects only", () => {
    const trace = useTraceRefreshGeneration();
    bumpTraceRefreshOnHistoryIngress(trace, { kind: "noop_duplicate_version" });
    expect(trace.traceRefreshGeneration.value).toBe(0);
    bumpTraceRefreshOnHistoryIngress(trace, { kind: "applied_delta" });
    expect(trace.traceRefreshGeneration.value).toBe(1);
    bumpTraceRefreshOnHistoryIngress(trace, { kind: "deferred", reason: "provenance_lags_live" });
    expect(trace.traceRefreshGeneration.value).toBe(1);
    bumpTraceRefreshOnHistoryIngress(trace, { kind: "applied_full" });
    expect(trace.traceRefreshGeneration.value).toBe(2);
  });
});
