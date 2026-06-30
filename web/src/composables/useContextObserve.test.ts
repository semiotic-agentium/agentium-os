// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { ref } from "vue";
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { useContextObserve } from "./useContextObserve";

describe("useContextObserve", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          contextId: "ctx-1",
          version: "v1",
          planning: null,
          llmOps: null,
          toolOps: null,
        }),
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("watch does not refetch when dependencies settle without scope change", async () => {
    const contextId = ref("ctx-1");
    const active = ref(false);

    useContextObserve({
      contextId,
      active,
    });

    await Promise.resolve();
    await Promise.resolve();
    const callsAfterMount = vi.mocked(fetch).mock.calls.length;

    await Promise.resolve();
    expect(vi.mocked(fetch).mock.calls.length).toBe(callsAfterMount);
  });

  it("refetches when scope key changes", async () => {
    const contextId = ref("ctx-1");
    const taskId = ref("task-1");
    const active = ref(false);

    useContextObserve({
      contextId,
      taskId,
      active,
    });

    await Promise.resolve();
    const callsAfterMount = vi.mocked(fetch).mock.calls.length;

    taskId.value = "task-2";
    await Promise.resolve();
    expect(vi.mocked(fetch).mock.calls.length).toBeGreaterThan(callsAfterMount);
  });

  it("force refresh bypasses dedupe when prior fetch completed", async () => {
    let resolveFetch!: (value: Response) => void;
    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          new Promise<Response>((resolve) => {
            resolveFetch = resolve;
          }),
      ),
    );

    const contextId = ref("ctx-1");
    const active = ref(false);

    const { refresh, loading } = useContextObserve({
      contextId,
      active,
    });

    await vi.waitFor(() => expect(vi.mocked(fetch).mock.calls.length).toBe(1));
    resolveFetch({
      ok: true,
      json: async () => ({
        contextId: "ctx-1",
        version: "v1",
        planning: null,
        llmOps: null,
        toolOps: null,
      }),
    } as Response);
    await vi.waitFor(() => expect(loading.value).toBe(false));

    refresh();
    await vi.waitFor(() => expect(vi.mocked(fetch).mock.calls.length).toBe(2));
  });

  it("force refresh coalesces while the same observe GET is in flight", async () => {
    let resolveFetch: (() => void) | undefined;
    const contextId = ref("ctx-1");
    const taskId = ref("dispatch-unit-abc");
    const agentPackage = ref("clickup-agent");
    const active = ref(false);

    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          new Promise<Response>((resolve) => {
            resolveFetch = () =>
              resolve({
                ok: true,
                json: async () => ({
                  contextId: "ctx-1",
                  version: "v1",
                  planning: null,
                  llmOps: null,
                  toolOps: null,
                }),
              } as Response);
          }),
      ),
    );

    const { refresh } = useContextObserve({
      contextId,
      taskId,
      agentPackage,
      active,
    });

    await Promise.resolve();
    expect(vi.mocked(fetch).mock.calls).toHaveLength(1);

    refresh();
    refresh();
    expect(vi.mocked(fetch).mock.calls).toHaveLength(1);

    resolveFetch?.();
    await Promise.resolve();
    await Promise.resolve();
  });

  it("includes includeDrift in scope key", async () => {
    const contextId = ref("ctx-1");
    const includeDrift = ref(false);
    const active = ref(false);

    useContextObserve({
      contextId,
      includeDrift,
      active,
    });

    await Promise.resolve();
    const callsAfterMount = vi.mocked(fetch).mock.calls.length;

    includeDrift.value = true;
    await Promise.resolve();
    expect(vi.mocked(fetch).mock.calls.length).toBeGreaterThan(callsAfterMount);
  });
});
