// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { computed, ref } from "vue";
import {
  clearInstanceCredentials,
  getInstanceUrl,
  instanceHostLabel,
  loadInstanceCredentialsFromStorage,
  normalizeInstanceUrl,
  setInstanceCredentials,
  verifyInstanceConnection,
} from "./instanceApi";

export type ConnectionStatus = "idle" | "connecting" | "connected" | "error";

const status = ref<ConnectionStatus>("idle");
const connectionError = ref<string | null>(null);
const draftUrl = ref("");
const draftToken = ref("");

function syncDraftFromStorage(): void {
  const { url, token } = loadInstanceCredentialsFromStorage();
  draftUrl.value = url;
  draftToken.value = token;
  if (url) status.value = "connected";
}

syncDraftFromStorage();

export function useInstanceClient() {
  const isConnected = computed(() => status.value === "connected" && Boolean(getInstanceUrl()));

  const hostLabel = computed(() => instanceHostLabel());

  async function connect(url?: string, token?: string): Promise<boolean> {
    const targetUrl = normalizeInstanceUrl(url ?? draftUrl.value);
    const targetToken = (token ?? draftToken.value).trim();
    if (!targetUrl) {
      connectionError.value = "Enter the Agentium OS instance URL (e.g. http://127.0.0.1:18080).";
      status.value = "error";
      return false;
    }
    status.value = "connecting";
    connectionError.value = null;
    const result = await verifyInstanceConnection(targetUrl, targetToken);
    if (!result.ok) {
      connectionError.value = result.error;
      status.value = "error";
      return false;
    }
    setInstanceCredentials(targetUrl, targetToken);
    draftUrl.value = targetUrl;
    draftToken.value = targetToken;
    status.value = "connected";
    return true;
  }

  async function tryAutoConnect(): Promise<boolean> {
    const { url, token } = loadInstanceCredentialsFromStorage();
    if (url) return connect(url, token);
    if (typeof window === "undefined") return false;
    return connect(window.location.origin, "");
  }

  function disconnect(): void {
    clearInstanceCredentials();
    draftUrl.value = "";
    draftToken.value = "";
    status.value = "idle";
    connectionError.value = null;
  }

  return {
    status,
    connectionError,
    draftUrl,
    draftToken,
    isConnected,
    hostLabel,
    connect,
    tryAutoConnect,
    disconnect,
  };
}
