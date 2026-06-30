// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { ref } from "vue";
import { instanceFetchJson } from "./instanceApi";
import { useDeployApi } from "./useDeployApi";
import type { PublishCommandPayload } from "../agent/sourceBundle";

export interface PublishResult {
  hash: string;
  version_ref?: { name: string; version: string | number };
}

export type LoadAgentPhase = "idle" | "validating" | "publishing" | "deploying" | "done" | "error";

const PUBLISH_TIMEOUT_MS = 600_000;

const phase = ref<LoadAgentPhase>("idle");
const error = ref<string | null>(null);
const lastHash = ref<string | null>(null);
const deployAfterPublish = ref(true);

export function usePublishApi() {
  async function publishSource(cmd: PublishCommandPayload): Promise<PublishResult | null> {
    phase.value = "publishing";
    error.value = null;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), PUBLISH_TIMEOUT_MS);
    try {
      const result = await instanceFetchJson<PublishResult>("/repository/publish", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(cmd),
        signal: controller.signal,
      }, 800);
      lastHash.value = result.hash;
      return result;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      phase.value = "error";
      return null;
    } finally {
      clearTimeout(timer);
    }
  }

  async function deployHash(hash: string): Promise<boolean> {
    phase.value = "deploying";
    error.value = null;
    const { deploy, error: deployError } = useDeployApi();
    const result = await deploy({ hash });
    if (!result) {
      error.value = deployError.value ?? "Deploy failed";
      phase.value = "error";
      return false;
    }
    return true;
  }

  async function loadAgent(cmd: PublishCommandPayload): Promise<PublishResult | null> {
    phase.value = "validating";
    error.value = null;
    const published = await publishSource(cmd);
    if (!published) return null;
    if (deployAfterPublish.value) {
      const ok = await deployHash(published.hash);
      if (!ok) return null;
    }
    phase.value = "done";
    return published;
  }

  function reset(): void {
    phase.value = "idle";
    error.value = null;
    lastHash.value = null;
  }

  return {
    phase,
    error,
    lastHash,
    deployAfterPublish,
    loadAgent,
    publishSource,
    deployHash,
    reset,
  };
}
