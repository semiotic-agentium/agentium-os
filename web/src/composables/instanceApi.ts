// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Shared HTTP client for a connected Agentium OS instance (server URL + optional operator token). */

/**
 * Layering (bottom → top):
 * - `instanceApi` — transport only (URL resolve, auth header, fetch). No Vue.
 * - `useInstanceClient` — connection session (connect/disconnect, host label).
 * - Domain composables — `useDeployApi`, `usePublishApi`, `useConfigApi`, `useAgentsApi`, …
 * - Vue components — consume domain composables; never import `instanceApi` directly.
 */

const STORAGE_URL = "agentium:instance-url";
const STORAGE_TOKEN = "agentium:runner-token";

let cachedUrl = "";
let cachedToken = "";

export class InstanceHttpError extends Error {
  readonly status: number;
  readonly body: string;

  constructor(status: number, body: string, maxLen = 500) {
    const clipped = body.slice(0, maxLen);
    super(`HTTP ${status}: ${clipped}`);
    this.name = "InstanceHttpError";
    this.status = status;
    this.body = clipped;
  }
}

export function loadInstanceCredentialsFromStorage(): { url: string; token: string } {
  if (typeof localStorage === "undefined") {
    return { url: "", token: "" };
  }
  cachedUrl = localStorage.getItem(STORAGE_URL)?.trim() ?? "";
  cachedToken = localStorage.getItem(STORAGE_TOKEN)?.trim() ?? "";
  return { url: cachedUrl, token: cachedToken };
}

export function getInstanceUrl(): string {
  if (!cachedUrl) loadInstanceCredentialsFromStorage();
  return cachedUrl;
}

export function getRunnerToken(): string {
  if (!cachedToken && typeof localStorage !== "undefined") {
    loadInstanceCredentialsFromStorage();
  }
  return cachedToken;
}

export function setInstanceCredentials(url: string, token: string): void {
  cachedUrl = url.trim().replace(/\/$/, "");
  cachedToken = token.trim();
  if (typeof localStorage !== "undefined") {
    if (cachedUrl) localStorage.setItem(STORAGE_URL, cachedUrl);
    else localStorage.removeItem(STORAGE_URL);
    if (cachedToken) localStorage.setItem(STORAGE_TOKEN, cachedToken);
    else localStorage.removeItem(STORAGE_TOKEN);
  }
}

export function clearInstanceCredentials(): void {
  setInstanceCredentials("", "");
}

/** Normalize user input to a base URL without trailing slash. */
export function normalizeInstanceUrl(raw: string): string {
  let s = raw.trim();
  if (!s) return "";
  if (!/^https?:\/\//i.test(s)) {
    s = `http://${s}`;
  }
  return s.replace(/\/$/, "");
}

/** Resolve an API path against the connected instance (relative when unset — co-served / Vite proxy). */
export function resolveInstanceUrl(path: string): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  const base = getInstanceUrl();
  if (!base) return p;
  return `${base.replace(/\/$/, "")}${p}`;
}

export function instanceAuthHeaders(init?: HeadersInit): Headers {
  const headers = new Headers(init);
  const token = getRunnerToken();
  if (token) headers.set("X-Runner-Token", token);
  return headers;
}

export async function instanceFetch(path: string, init?: RequestInit): Promise<Response> {
  return fetch(resolveInstanceUrl(path), {
    ...init,
    headers: instanceAuthHeaders(init?.headers),
  });
}

export async function readInstanceErrorBody(
  response: Response,
  maxLen = 500,
): Promise<string> {
  try {
    return (await response.text()).slice(0, maxLen);
  } catch {
    return response.statusText || "Request failed";
  }
}

export async function instanceFetchJson<T>(
  path: string,
  init?: RequestInit,
  errorBodyMaxLen = 500,
): Promise<T> {
  const res = await instanceFetch(path, init);
  if (!res.ok) {
    throw new InstanceHttpError(res.status, await readInstanceErrorBody(res, errorBodyMaxLen), errorBodyMaxLen);
  }
  return (await res.json()) as T;
}

export async function instanceFetchText(
  path: string,
  init?: RequestInit,
): Promise<string> {
  const res = await instanceFetch(path, init);
  if (!res.ok) {
    throw new InstanceHttpError(res.status, await readInstanceErrorBody(res), 500);
  }
  return res.text();
}

export async function verifyInstanceConnection(
  url: string,
  token: string,
): Promise<{ ok: true } | { ok: false; error: string }> {
  const base = normalizeInstanceUrl(url);
  if (!base) return { ok: false, error: "Instance URL is required." };
  try {
    const headers = new Headers();
    if (token.trim()) headers.set("X-Runner-Token", token.trim());
    const res = await fetch(`${base}/healthz`, { method: "GET", headers });
    if (res.ok || res.status === 404) {
      const agents = await fetch(`${base}/agents`, { method: "GET", headers });
      if (agents.ok) return { ok: true };
      return { ok: false, error: `Agents discovery failed (${agents.status}).` };
    }
    return { ok: false, error: `Health check failed (${res.status}).` };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export function instanceHostLabel(): string {
  const url = getInstanceUrl();
  if (!url) {
    return typeof window !== "undefined" ? window.location.host : "";
  }
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
