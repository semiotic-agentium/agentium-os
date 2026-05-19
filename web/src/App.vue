<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, onMounted, onUnmounted, watch, ref } from "vue";
import AgentSelector from "./components/AgentSelector.vue";
import ChatTabs from "./components/ChatTabs.vue";
import ChatWindow from "./components/ChatWindow.vue";
import ConversationHistorySelector from "./components/ConversationHistorySelector.vue";
import Dashboard from "./components/Dashboard.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import ErrorBoundary from "./components/ErrorBoundary.vue";
import Navbar from "./components/Navbar.vue";
import ProvenancePane from "./components/ProvenancePane.vue";
import SettingsView from "./components/SettingsView.vue";
import EventConsole from "./components/events/EventConsole.vue";
import ToastContainer from "./components/ToastContainer.vue";
import { useProvenanceOps } from "./composables/useProvenanceOps";
import { useChatTabs } from "./composables/useChatTabs";
import { useTheme } from "./composables/useTheme";
import { useConfirm } from "./composables/useConfirm";
import { parseMermaidBlocks } from "./utils/parseMermaid";
import type { AgentDiscoveryEntry, ChatMessage, LlmPromptOperation } from "./types/a2a";
import type { ProvenancePaneTab } from "./composables/useDashboardViewModel";

/** First inline mermaid block in agent messages (stops at first hit). */
function firstInlineMermaidDiagram(messages: ChatMessage[]): string | null {
  for (const m of messages) {
    if (m.role !== "agent") continue;
    const blocks = parseMermaidBlocks(m.text);
    if (blocks.length > 0) return blocks[0]!;
  }
  return null;
}

const {
  tabs,
  activeTabId,
  activeClient,
  createTab,
  closeTab,
  switchTab,
  renameTab,
} = useChatTabs();

// Derived refs from the active tab's client
const agents = computed(() => activeClient.value?.agents.value ?? []);
const selectedAgent = computed(() => activeClient.value?.selectedAgent.value ?? null);
const messages = computed(() => activeClient.value?.messages.value ?? []);
const isLoading = computed(() => activeClient.value?.isLoading.value ?? false);
const provenanceDiagram = computed(() => activeClient.value?.provenanceDiagram.value ?? "");
const traceRefreshGeneration = computed(() => activeClient.value?.traceRefreshGeneration.value ?? 0);
const contextMetrics = computed(() => activeClient.value?.contextMetrics.value ?? null);
const promptMessageCharsSessionCurrent = computed(
  () => activeClient.value?.promptMessageCharsSessionCurrent.value ?? null,
);
const llmPromptOperations = computed<LlmPromptOperation[]>(
  () => activeClient.value?.llmPromptOperations.value ?? [],
);
const conversationHistoryOptions = computed(
  () => activeClient.value?.conversationHistoryOptions.value ?? [],
);
const selectedHistoryContextId = computed(
  () => activeClient.value?.selectedHistoryContextId.value ?? null,
);
const historyLoading = computed(() => activeClient.value?.historyLoading.value ?? false);
const historyHydrateState = computed(() => activeClient.value?.historyHydrateState.value ?? "idle");
const contextId = computed(() => activeClient.value?.contextId.value ?? undefined);
const taskId = computed(() => activeClient.value?.taskId.value ?? null);

const dashboardLaneSnapshots = computed(() =>
  tabs.value.map((t) => ({
    tabId: t.id,
    title: t.title,
    contextId: t.client.contextId.value,
    agent: t.client.selectedAgent.value,
    isStreaming: t.client.isLoading.value,
    isActive: t.id === activeTabId.value,
  })),
);

const provenanceExternalFocus = ref<{ nonce: number; tab: ProvenancePaneTab } | undefined>();

function onDashboardGoChat(payload?: { tabId?: string; provenanceTab?: ProvenancePaneTab }) {
  if (payload?.tabId) {
    switchTab(payload.tabId);
  }
  view.value = "chat";
  if (payload?.provenanceTab) {
    provenanceExternalFocus.value = { nonce: Date.now(), tab: payload.provenanceTab };
  }
}
const workflowProgress = computed(() => activeClient.value?.workflowProgress.value ?? { phase: "idle" as const, nodes: [], completedNodes: [] });
const awaitingInput = computed(() => activeClient.value?.awaitingInput.value ?? false);
const inputRequiredPrompt = computed(() => activeClient.value?.inputRequiredPrompt.value ?? "");
function normalizeChatTitle(text: string): string {
  const compact = text.replace(/\s+/g, " ").trim();
  if (!compact) return "New Chat";
  return compact.length > 48 ? `${compact.slice(0, 45)}...` : compact;
}
function maybeSetActiveTabTitle(title: string, force = false) {
  if (!activeTabId.value) return;
  const tab = tabs.value.find((t) => t.id === activeTabId.value);
  if (!tab) return;
  if (!force && tab.title !== "New Chat") return;
  renameTab(tab.id, normalizeChatTitle(title));
}
function selectAgent(agent: AgentDiscoveryEntry) {
  activeClient.value?.selectAgent(agent);
}
function sendMessage(text: string) {
  maybeSetActiveTabTitle(text);
  activeClient.value?.sendMessage(text);
}
function cancelStream() { activeClient.value?.cancelStream(); }
function refreshConversationHistories() {
  activeClient.value?.fetchConversationHistoryOptions();
}
function selectConversationHistory(option: { contextId: string; taskId?: string | null; preview?: string }) {
  maybeSetActiveTabTitle(option.preview ?? option.contextId, true);
  activeClient.value?.loadConversationHistoryContext(option.contextId);
}

const { confirm } = useConfirm();

async function handleSelectAgent(agent: AgentDiscoveryEntry): Promise<void> {
  if (messages.value.length > 0 && agent.agent_package !== selectedAgent.value?.agent_package) {
    const ok = await confirm(
      "Switch agent?",
      "Switching agents will clear the current conversation.",
    );
    if (!ok) return;
  }
  selectAgent(agent);
}
const { theme, toggle: toggleTheme } = useTheme();
const { createQuery } = useProvenanceOps();

// Active view — chat is the landing page; dashboard is a debug/metrics surface
const view = ref<"dashboard" | "chat" | "events" | "settings">("chat");

// Traces pane: hidden by default; persisted so power users keep it open across reloads
const TRACES_STORAGE_KEY = "agentium:showTraces";
const showTraces = ref<boolean>(
  typeof localStorage !== "undefined" && localStorage.getItem(TRACES_STORAGE_KEY) === "1",
);
function toggleTraces() {
  showTraces.value = !showTraces.value;
  if (typeof localStorage !== "undefined") {
    localStorage.setItem(TRACES_STORAGE_KEY, showTraces.value ? "1" : "0");
  }
}
const isApplyingRouteState = ref(false);
let lastRouteKey = "";

type ViewName = "dashboard" | "chat" | "events" | "settings";
type UiRouteState = {
  view: ViewName;
  agentPackage: string | null;
  agentInstance: string | null;
  contextId: string | null;
};

function parseView(raw: string | null): ViewName {
  if (raw === "chat" || raw === "events" || raw === "settings" || raw === "dashboard") {
    return raw;
  }
  return "chat";
}

function readRouteStateFromUrl(): UiRouteState {
  const params = new URLSearchParams(window.location.search);
  return {
    view: parseView(params.get("view")),
    agentPackage: params.get("agentPackage"),
    agentInstance: params.get("agentInstance"),
    contextId: params.get("contextId"),
  };
}

function routeStateKey(state: UiRouteState): string {
  return JSON.stringify(state);
}

function currentRouteState(): UiRouteState {
  return {
    view: view.value,
    agentPackage: selectedAgent.value?.agent_package ?? null,
    agentInstance: selectedAgent.value?.agent_instance_id ?? null,
    contextId: selectedHistoryContextId.value ?? contextId.value ?? null,
  };
}

function writeRouteState(push: boolean): void {
  if (isApplyingRouteState.value) return;
  const state = currentRouteState();
  const key = routeStateKey(state);
  if (key === lastRouteKey) return;
  const url = new URL(window.location.href);
  const params = new URLSearchParams(url.search);
  params.set("view", state.view);
  if (state.agentPackage) params.set("agentPackage", state.agentPackage);
  else params.delete("agentPackage");
  if (state.agentInstance) params.set("agentInstance", state.agentInstance);
  else params.delete("agentInstance");
  if (state.contextId) params.set("contextId", state.contextId);
  else params.delete("contextId");
  url.search = params.toString();

  if (push) {
    window.history.pushState(state, "", url.toString());
  } else {
    window.history.replaceState(state, "", url.toString());
  }
  lastRouteKey = key;
}

async function applyRouteStateFromUrl(): Promise<void> {
  const next = readRouteStateFromUrl();
  const nextKey = routeStateKey(next);
  if (nextKey === lastRouteKey) return;
  isApplyingRouteState.value = true;
  try {
    view.value = next.view;
    if (activeClient.value) {
      await activeClient.value.fetchAgents();
    }
    if (next.agentPackage && activeClient.value?.agents.value.length) {
      const match = activeClient.value.agents.value.find(
        (a) =>
          a.agent_package === next.agentPackage &&
          (!next.agentInstance || a.agent_instance_id === next.agentInstance),
      );
      if (match) {
        const cur = activeClient.value.selectedAgent.value;
        const sameAgent =
          cur?.agent_package === match.agent_package &&
          cur?.agent_instance_id === match.agent_instance_id;
        // selectAgent clears the transcript; skip when URL already matches the active agent so we
        // do not wipe messages before loadConversationHistoryContext runs (or on redundant applies).
        if (!sameAgent) {
          activeClient.value.selectAgent(match);
        }
      }
    }
    if (next.contextId && activeClient.value) {
      await activeClient.value.loadConversationHistoryContext(next.contextId);
    }
    lastRouteKey = nextKey;
  } finally {
    isApplyingRouteState.value = false;
  }
}

function onPopState(): void {
  void applyRouteStateFromUrl();
}

watch(
  [
    () => activeClient.value?.clientId ?? null,
    () => selectedAgent.value?.agent_package ?? null,
    () => selectedAgent.value?.agent_instance_id ?? null,
    () => selectedHistoryContextId.value ?? contextId.value ?? null,
    () => messages.value.length,
  ],
  ([clientId, agentPackage, agentInstance, currentContextId, messageCount]) => {
    if (window.location.hostname !== "localhost") return;
    console.debug(
      "[transcript]",
      JSON.stringify({
        activeClientId: clientId,
        agentPackage,
        agentInstance,
        contextId: currentContextId,
        messageCount,
      }),
    );
  },
  { immediate: true },
);

// Trace pane only displays the first diagram; avoid parsing/rendering all agent mermaid blocks.
const provenancePaneDiagrams = computed(() => {
  const prov = provenanceDiagram.value.trim();
  if (prov.length > 0) return [prov];
  const first = firstInlineMermaidDiagram(messages.value);
  return first ? [first] : [];
});

const dashboardOps = createQuery("llm_calls", {
  pageSize: 30,
  groupBy: ["agent_id", "agent_package", "agent_version", "model"],
  sortBy: "timestamp_ms",
  sortDir: "desc",
});
const dashboardOpsTool = createQuery("tool_calls", {
  pageSize: 30,
  groupBy: ["agent_id", "agent_package", "agent_version", "tool_name"],
  sortBy: "timestamp_ms",
  sortDir: "desc",
});

watch(
  () => contextMetrics.value?.context_id,
  async () => {
    const contextId = contextMetrics.value?.context_id;
    if (!contextId) return;
    await Promise.all([
      dashboardOps.run({ contextId, outcome: "both" }),
      dashboardOpsTool.run({ contextId, outcome: "both" }),
    ]);
  },
  { immediate: true },
);

const provenanceDashboardSummary = computed(() => {
  const llm = dashboardOps.state.value.response?.summary;
  const tool = dashboardOpsTool.state.value.response?.summary;
  const llmGroups = dashboardOps.state.value.response?.hotspotGroups ?? [];
  const toolGroups = dashboardOpsTool.state.value.response?.hotspotGroups ?? [];
  return {
    count: (llm?.count ?? 0) + (tool?.count ?? 0),
    failedCount: (llm?.failedCount ?? 0) + (tool?.failedCount ?? 0),
    durationMsTotal: (llm?.durationMsTotal ?? 0) + (tool?.durationMsTotal ?? 0),
    totalTokens: (llm?.totalTokens ?? 0) + (tool?.totalTokens ?? 0),
    llmCount: llm?.count ?? 0,
    toolCount: tool?.count ?? 0,
    hotspotGroups: [...llmGroups, ...toolGroups].slice(0, 8),
    lastUpdatedAt: Math.max(
      dashboardOps.state.value.lastUpdatedAt ?? 0,
      dashboardOpsTool.state.value.lastUpdatedAt ?? 0,
    ),
  };
});

// System health check
const systemOnline = ref(true);
let healthTimer: ReturnType<typeof setInterval> | null = null;

async function checkHealth() {
  try {
    const res = await fetch("/agents", { method: "GET" });
    systemOnline.value = res.ok;
  } catch {
    systemOnline.value = false;
  }
}

onMounted(() => {
  void applyRouteStateFromUrl().then(() => {
    writeRouteState(false);
  });
  checkHealth();
  healthTimer = setInterval(checkHealth, 30_000);
  window.addEventListener("popstate", onPopState);
});

onUnmounted(() => {
  if (healthTimer) clearInterval(healthTimer);
  window.removeEventListener("popstate", onPopState);
});

watch(
  [
    view,
    () => activeTabId.value,
    () => selectedAgent.value?.agent_package ?? null,
    () => selectedAgent.value?.agent_instance_id ?? null,
    () => selectedHistoryContextId.value ?? contextId.value ?? null,
  ],
  () => {
    writeRouteState(true);
  },
);
</script>

<template>
  <div class="app">
    <a href="#main-content" class="sr-only-focusable">Skip to main content</a>
    <Navbar
      :view="view"
      :agent-count="agents.length"
      :theme="theme"
      :system-online="systemOnline"
      @change-view="view = $event"
      @toggle-theme="toggleTheme"
    />

    <div id="main-content" class="app-content-area">
      <ErrorBoundary v-if="view === 'dashboard'">
        <Dashboard
          :agents="agents"
          :lane-snapshots="dashboardLaneSnapshots"
          :runner-online="systemOnline"
          :context-id="contextId"
          :context-metrics="contextMetrics"
          :prompt-message-chars-session-current="promptMessageCharsSessionCurrent"
          :provenance-diagram="provenanceDiagram"
          :messages="messages"
          :provenance-summary="provenanceDashboardSummary"
          @open-settings="view = 'settings'"
          @go-chat="onDashboardGoChat"
        />
      </ErrorBoundary>

      <ErrorBoundary v-else-if="view === 'chat'">
        <div class="chat-layout">
        <div class="chat-toolbar">
          <ChatTabs
            :tabs="tabs"
            :active-tab-id="activeTabId"
            @switch="switchTab"
            @close="closeTab"
            @create="createTab()"
          />
          <AgentSelector :agents="agents" :selected="selectedAgent" @select="handleSelectAgent" />
          <ConversationHistorySelector
            v-if="selectedAgent"
            :histories="conversationHistoryOptions"
            :selected-context-id="selectedHistoryContextId"
            :loading="historyLoading"
            @select="selectConversationHistory"
            @refresh="refreshConversationHistories"
          />
          <button
            type="button"
            class="traces-toggle"
            :class="{ 'traces-toggle--on': showTraces }"
            :aria-pressed="showTraces"
            :title="showTraces ? 'Hide traces panel' : 'Show traces panel'"
            @click="toggleTraces"
          >
            {{ showTraces ? "Hide traces" : "Show traces" }}
          </button>
        </div>

        <div class="app-body">
          <ChatWindow
            :messages="messages"
            :is-loading="isLoading"
            :disabled="!selectedAgent"
            :awaiting-input="awaitingInput"
            :input-required-prompt="inputRequiredPrompt"
            :workflow-progress="workflowProgress"
            :history-hydrate-state="historyHydrateState"
            :selected-context-id="selectedHistoryContextId"
            :agents="agents"
            @send="sendMessage"
            @cancel="cancelStream"
            @select-agent="handleSelectAgent"
            @open-settings="view = 'settings'"
          />
          <ProvenancePane
            v-if="showTraces"
            :context-id="contextId"
            :task-id="taskId ?? undefined"
            :selected-agent-id="undefined"
            :is-streaming="isLoading"
            :diagrams="provenancePaneDiagrams"
            :trace-refresh-tick="traceRefreshGeneration"
            :llm-prompt-operations="llmPromptOperations"
            :external-tab-focus="provenanceExternalFocus"
          />
        </div>
      </div>
      </ErrorBoundary>

      <ErrorBoundary v-else-if="view === 'events'">
        <EventConsole />
      </ErrorBoundary>

      <ErrorBoundary v-else-if="view === 'settings'">
        <SettingsView />
      </ErrorBoundary>
    </div>
    <ToastContainer />
    <ConfirmDialog />
  </div>
</template>
