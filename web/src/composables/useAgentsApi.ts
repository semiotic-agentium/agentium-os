// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { instanceFetch, instanceFetchJson } from "./instanceApi";
import type { AgentDiscoveryEntry } from "../types/a2a";

/** Agent discovery and lightweight instance reachability checks. */
export function useAgentsApi() {
  async function fetchAgents(): Promise<AgentDiscoveryEntry[]> {
    try {
      return await instanceFetchJson<AgentDiscoveryEntry[]>("/agents");
    } catch {
      return [];
    }
  }

  async function pingInstance(): Promise<boolean> {
    try {
      const res = await instanceFetch("/agents", { method: "GET" });
      return res.ok;
    } catch {
      return false;
    }
  }

  return { fetchAgents, pingInstance };
}
