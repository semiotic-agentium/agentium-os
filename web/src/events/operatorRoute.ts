// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Scoped URL params for chat vs Event Console (legacy `agentPackage` fallback). */

export type ViewName = "dashboard" | "agents" | "chat" | "events" | "settings";

export type ChatRouteState = {
  agentPackage: string | null;
  agentInstance: string | null;
  contextId: string | null;
};

export type EventConsoleRouteState = {
  agentPackage: string | null;
  agentInstance: string | null;
  contextId: string | null;
};

const LEGACY_AGENT = "agentPackage";
const LEGACY_INSTANCE = "agentInstance";
const LEGACY_CONTEXT = "contextId";

const CHAT_AGENT = "chatAgentPackage";
const CHAT_INSTANCE = "chatAgentInstance";
const CHAT_CONTEXT = "chatContextId";

const EVENT_AGENT = "eventAgentPackage";
const EVENT_INSTANCE = "eventAgentInstance";
const EVENT_CONTEXT = "eventContextId";

export function parseView(raw: string | null): ViewName {
  if (
    raw === "chat" ||
    raw === "agents" ||
    raw === "events" ||
    raw === "settings" ||
    raw === "dashboard"
  ) {
    return raw;
  }
  return "chat";
}

function readView(params: URLSearchParams): ViewName {
  return parseView(params.get("view"));
}

/** Read chat-scoped agent/context from the URL (active view must be chat for legacy fallback). */
export function readChatRouteFromUrl(): ChatRouteState {
  if (typeof window === "undefined") {
    return { agentPackage: null, agentInstance: null, contextId: null };
  }
  const params = new URLSearchParams(window.location.search);
  const view = readView(params);
  const legacyOk = view === "chat" || view === "dashboard";
  return {
    agentPackage:
      params.get(CHAT_AGENT) ?? (legacyOk ? params.get(LEGACY_AGENT) : null),
    agentInstance:
      params.get(CHAT_INSTANCE) ?? (legacyOk ? params.get(LEGACY_INSTANCE) : null),
    contextId:
      params.get(CHAT_CONTEXT) ?? (legacyOk ? params.get(LEGACY_CONTEXT) : null),
  };
}

/** Write chat-scoped params; only mutates URL when `view` is chat. */
export function writeChatRouteToUrl(
  patch: Partial<ChatRouteState>,
  options?: { push?: boolean },
): void {
  if (typeof window === "undefined") return;
  const params = new URLSearchParams(window.location.search);
  const view = readView(params);
  if (view !== "chat") return;

  params.set("view", "chat");

  if (patch.agentPackage !== undefined) {
    if (patch.agentPackage) params.set(CHAT_AGENT, patch.agentPackage);
    else params.delete(CHAT_AGENT);
    params.delete(LEGACY_AGENT);
  }
  if (patch.agentInstance !== undefined) {
    if (patch.agentInstance) params.set(CHAT_INSTANCE, patch.agentInstance);
    else params.delete(CHAT_INSTANCE);
    params.delete(LEGACY_INSTANCE);
  }
  if (patch.contextId !== undefined) {
    if (patch.contextId) params.set(CHAT_CONTEXT, patch.contextId);
    else params.delete(CHAT_CONTEXT);
    params.delete(LEGACY_CONTEXT);
  }

  const url = new URL(window.location.href);
  url.search = params.toString();
  if (options?.push) {
    window.history.pushState(window.history.state, "", url.toString());
  } else {
    window.history.replaceState(window.history.state, "", url.toString());
  }
}

/** Read event-console-scoped agent/context from the URL. */
export function readEventConsoleRouteFromUrl(): EventConsoleRouteState {
  if (typeof window === "undefined") {
    return { agentPackage: null, agentInstance: null, contextId: null };
  }
  const params = new URLSearchParams(window.location.search);
  const view = readView(params);
  const legacyOk = view === "events";
  return {
    agentPackage:
      params.get(EVENT_AGENT) ?? (legacyOk ? params.get(LEGACY_AGENT) : null),
    agentInstance:
      params.get(EVENT_INSTANCE) ?? (legacyOk ? params.get(LEGACY_INSTANCE) : null),
    contextId:
      params.get(EVENT_CONTEXT) ?? (legacyOk ? params.get(LEGACY_CONTEXT) : null),
  };
}

/** Write event-console-scoped params and set `view=events`. */
export function writeEventConsoleRouteToUrl(
  patch: Partial<EventConsoleRouteState>,
): void {
  if (typeof window === "undefined") return;
  const url = new URL(window.location.href);
  const params = url.searchParams;
  params.set("view", "events");

  if (patch.agentPackage !== undefined) {
    if (patch.agentPackage) params.set(EVENT_AGENT, patch.agentPackage);
    else params.delete(EVENT_AGENT);
    params.delete(LEGACY_AGENT);
  }
  if (patch.agentInstance !== undefined) {
    if (patch.agentInstance) params.set(EVENT_INSTANCE, patch.agentInstance);
    else params.delete(EVENT_INSTANCE);
    params.delete(LEGACY_INSTANCE);
  }
  if (patch.contextId !== undefined) {
    if (patch.contextId) params.set(EVENT_CONTEXT, patch.contextId);
    else params.delete(EVENT_CONTEXT);
    params.delete(LEGACY_CONTEXT);
  }

  window.history.replaceState(window.history.state, "", url.toString());
}

export function chatRouteKey(state: ChatRouteState & { view?: ViewName }): string {
  return JSON.stringify({
    view: state.view ?? "chat",
    agentPackage: state.agentPackage,
    agentInstance: state.agentInstance,
    contextId: state.contextId,
  });
}
