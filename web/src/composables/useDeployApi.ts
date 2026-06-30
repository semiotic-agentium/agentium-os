// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { ref } from "vue";
import { instanceFetch, instanceFetchJson, readInstanceErrorBody } from "./instanceApi";

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

/** Shared fleet state — one list for Settings, Agents view, and publish→deploy flows. */
const deployments = ref<DeploymentRecord[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);

async function fetchDeployments(): Promise<void> {
  loading.value = true;
  error.value = null;
  try {
    deployments.value = await instanceFetchJson<DeploymentRecord[]>("/deployments");
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    loading.value = false;
  }
}

async function deploy(
  request: DeployRequest,
): Promise<{ hash: string; already_deployed: boolean } | null> {
  error.value = null;
  try {
    const result = await instanceFetchJson<{ hash: string; already_deployed: boolean }>("/deploy", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(request),
    });
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
    const res = await instanceFetch("/undeploy", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ hash }),
    });
    if (!res.ok) {
      throw new Error(`HTTP ${res.status}: ${await readInstanceErrorBody(res)}`);
    }
    await fetchDeployments();
    return true;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
    return false;
  }
}

export function useDeployApi() {
  return { deployments, loading, error, fetchDeployments, deploy, undeploy };
}
