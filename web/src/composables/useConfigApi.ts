// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { ref } from "vue";
import type {
  ConfigVersionDto,
  SecretOverviewEntryDto,
  SecretRequestDto,
  ToolConfigDto,
  ToolConfigSchemaDto,
} from "../types/config";

export interface ConfigApiError {
  status: number;
  title: string;
  detail?: string;
}

async function parseProblem(response: Response): Promise<ConfigApiError> {
  const text = await response.text();
  let detail = text;
  try {
    const json = JSON.parse(text) as { title?: string; detail?: string };
    detail = json.detail ?? json.title ?? text;
  } catch {
    // use raw text
  }
  return {
    status: response.status,
    title: response.statusText || "Error",
    detail: detail || undefined,
  };
}

export function useConfigApi() {
  const loading = ref(false);

  async function fetchConfigList(): Promise<
    { data: ToolConfigSchemaDto[] } | { error: ConfigApiError }
  > {
    loading.value = true;
    try {
      const res = await fetch("/config");
      if (res.status === 503) {
        return {
          error: {
            status: 503,
            title: "Service Unavailable",
            detail: "Config service is not available",
          },
        };
      }
      if (!res.ok) {
        return { error: await parseProblem(res) };
      }
      const contentType = res.headers.get("content-type") ?? "";
      if (!contentType.includes("application/json")) {
        return {
          error: {
            status: res.status,
            title: "Invalid Response",
            detail:
              "Config service is not available. Is the runner serving this app with config enabled?",
          },
        };
      }
      const data = (await res.json()) as ToolConfigSchemaDto[];
      return { data };
    } catch (e) {
      if (e instanceof SyntaxError) {
        return {
          error: {
            status: 0,
            title: "Invalid Response",
            detail:
              "Config service is not available. Is the runner serving this app with config enabled?",
          },
        };
      }
      throw e;
    } finally {
      loading.value = false;
    }
  }

  async function fetchConfig(
    bundleName: string,
  ): Promise<{ data: ToolConfigDto } | { error: ConfigApiError }> {
    loading.value = true;
    try {
      const res = await fetch(`/config/${encodeURIComponent(bundleName)}`);
      if (res.status === 503) {
        return {
          error: {
            status: 503,
            title: "Service Unavailable",
            detail: "Config service is not available",
          },
        };
      }
      if (!res.ok) {
        return { error: await parseProblem(res) };
      }
      const contentType = res.headers.get("content-type") ?? "";
      if (!contentType.includes("application/json")) {
        return {
          error: {
            status: res.status,
            title: "Invalid Response",
            detail: "Config service is not available.",
          },
        };
      }
      const data = (await res.json()) as ToolConfigDto;
      return { data };
    } catch (e) {
      if (e instanceof SyntaxError) {
        return {
          error: {
            status: 0,
            title: "Invalid Response",
            detail: "Config service is not available.",
          },
        };
      }
      throw e;
    } finally {
      loading.value = false;
    }
  }

  async function putConfig(
    bundleName: string,
    body: Record<string, unknown>,
    expectedVersion?: number,
  ): Promise<{ data: ConfigVersionDto } | { error: ConfigApiError }> {
    loading.value = true;
    try {
      const hdrs: Record<string, string> = { "Content-Type": "application/json" };
      if (expectedVersion !== undefined) {
        hdrs["If-Match"] = String(expectedVersion);
      }
      const res = await fetch(`/config/${encodeURIComponent(bundleName)}`, {
        method: "PUT",
        headers: hdrs,
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        return { error: await parseProblem(res) };
      }
      const data = (await res.json()) as ConfigVersionDto;
      return { data };
    } finally {
      loading.value = false;
    }
  }

  async function fetchSecretRequests(
    toolName: string,
  ): Promise<{ data: SecretRequestDto[] } | { error: ConfigApiError }> {
    try {
      const res = await fetch(`/config/${encodeURIComponent(toolName)}/secret-requests`);
      if (!res.ok) {
        return { error: await parseProblem(res) };
      }
      const data = (await res.json()) as SecretRequestDto[];
      return { data };
    } catch (e) {
      return {
        error: {
          status: 0,
          title: "Network Error",
          detail: e instanceof Error ? e.message : String(e),
        },
      };
    }
  }

  async function putSecret(
    name: string,
    linkFrom: string,
  ): Promise<{ success: true } | { error: ConfigApiError }> {
    try {
      const res = await fetch(`/config/secrets/${encodeURIComponent(name)}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ link_from: linkFrom.trim() }),
      });
      if (res.status === 204) {
        return { success: true };
      }
      return { error: await parseProblem(res) };
    } catch (e) {
      return {
        error: {
          status: 0,
          title: "Network Error",
          detail: e instanceof Error ? e.message : String(e),
        },
      };
    }
  }

  async function deleteSecret(
    name: string,
  ): Promise<{ success: true } | { error: ConfigApiError }> {
    try {
      const res = await fetch(`/config/secrets/${encodeURIComponent(name)}`, {
        method: "DELETE",
      });
      if (res.status === 204) {
        return { success: true };
      }
      return { error: await parseProblem(res) };
    } catch (e) {
      return {
        error: {
          status: 0,
          title: "Network Error",
          detail: e instanceof Error ? e.message : String(e),
        },
      };
    }
  }

  async function fetchStoreKeys(): Promise<{ data: string[] } | { error: ConfigApiError }> {
    try {
      const res = await fetch("/config/secrets/store-keys");
      if (!res.ok) {
        return { error: await parseProblem(res) };
      }
      const data = (await res.json()) as string[];
      return { data };
    } catch (e) {
      return {
        error: {
          status: 0,
          title: "Network Error",
          detail: e instanceof Error ? e.message : String(e),
        },
      };
    }
  }

  async function fetchSecretsOverview(): Promise<
    { data: SecretOverviewEntryDto[] } | { error: ConfigApiError }
  > {
    loading.value = true;
    try {
      const res = await fetch("/config/secrets-overview");
      if (res.status === 503) {
        return {
          error: {
            status: 503,
            title: "Service Unavailable",
            detail: "Tool catalog is not available",
          },
        };
      }
      if (!res.ok) {
        return { error: await parseProblem(res) };
      }
      const contentType = res.headers.get("content-type") ?? "";
      if (!contentType.includes("application/json")) {
        return {
          error: {
            status: res.status,
            title: "Invalid Response",
            detail: "Secrets overview is not available.",
          },
        };
      }
      const data = (await res.json()) as SecretOverviewEntryDto[];
      return { data };
    } catch (e) {
      if (e instanceof SyntaxError) {
        return {
          error: {
            status: 0,
            title: "Invalid Response",
            detail: "Service is not available.",
          },
        };
      }
      throw e;
    } finally {
      loading.value = false;
    }
  }

  async function fetchConfigVersions(
    bundleName: string,
  ): Promise<{ data: ConfigVersionDto[] } | { error: ConfigApiError }> {
    loading.value = true;
    try {
      const res = await fetch(`/config/${encodeURIComponent(bundleName)}/versions`);
      if (!res.ok) return { error: await parseProblem(res) };
      const data = (await res.json()) as ConfigVersionDto[];
      return { data };
    } catch (e) {
      return {
        error: {
          status: 0,
          title: "Network Error",
          detail: e instanceof Error ? e.message : String(e),
        },
      };
    } finally {
      loading.value = false;
    }
  }

  return {
    loading,
    fetchConfigList,
    fetchConfig,
    fetchConfigVersions,
    putConfig,
    fetchSecretRequests,
    fetchSecretsOverview,
    fetchStoreKeys,
    putSecret,
    deleteSecret,
  };
}
