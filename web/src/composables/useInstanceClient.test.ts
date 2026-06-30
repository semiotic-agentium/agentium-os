// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  clearInstanceCredentials,
  getInstanceUrl,
  getRunnerToken,
  setInstanceCredentials,
} from "./instanceApi";
import { useInstanceClient } from "./useInstanceClient";

describe("useInstanceClient", () => {
  beforeEach(() => {
    clearInstanceCredentials();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: async () => [],
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    clearInstanceCredentials();
  });

  it("starts disconnected when storage is empty and auto-connect fails", async () => {
    vi.mocked(fetch).mockResolvedValueOnce({
      ok: false,
      status: 503,
    } as Response);

    const { isConnected, tryAutoConnect } = useInstanceClient();
    const ok = await tryAutoConnect();

    expect(ok).toBe(false);
    expect(isConnected.value).toBe(false);
  });

  it("connect stores credentials and marks session connected", async () => {
    const { connect, isConnected, hostLabel } = useInstanceClient();
    const ok = await connect("http://127.0.0.1:18080", "secret-token");

    expect(ok).toBe(true);
    expect(isConnected.value).toBe(true);
    expect(hostLabel.value).toBe("127.0.0.1:18080");
    expect(getInstanceUrl()).toBe("http://127.0.0.1:18080");
    expect(getRunnerToken()).toBe("secret-token");
  });

  it("disconnect clears credentials and connection state", async () => {
    setInstanceCredentials("http://127.0.0.1:18080", "tok");
    const { disconnect, isConnected } = useInstanceClient();

    disconnect();

    expect(isConnected.value).toBe(false);
    expect(getInstanceUrl()).toBe("");
    expect(getRunnerToken()).toBe("");
  });
});
