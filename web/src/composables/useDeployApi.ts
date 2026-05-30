// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { ref } from "vue";

export interface DeploymentRecord {
  content_hash: string;
  agent_name: string;
  deployed_at: string;
  status: string;
  last_error?: string;
  last_attempt_at?: string;
  failure_count: number;
}

export interface DeployRequest {
  hash?: string;
  name?: string;
  version?: string;
}

export function useDeployApi() {
  const deployments = ref<DeploymentRecord[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  async function fetchDeployments() {
    loading.value = true;
    error.value = null;
    try {
      const res = await fetch("/deployments");
      if (!res.ok) throw new Error(`Failed to fetch deployments: ${res.status}`);
      deployments.value = await res.json();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
    } finally {
      loading.value = false;
    }
  }

  async function deploy(request: DeployRequest): Promise<{ hash: string; already_deployed: boolean } | null> {
    error.value = null;
    try {
      const res = await fetch("/deploy", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(`Deploy failed (${res.status}): ${text}`);
      }
      const result = await res.json();
      await fetchDeployments();
      return result;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      return null;
    }
  }

  async function undeploy(hash: string): Promise<boolean> {
    error.value = null;
    try {
      const res = await fetch("/undeploy", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ hash }),
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(`Undeploy failed (${res.status}): ${text}`);
      }
      await fetchDeployments();
      return true;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      return false;
    }
  }

  return { deployments, loading, error, fetchDeployments, deploy, undeploy };
}
