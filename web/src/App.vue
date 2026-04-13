<script setup lang="ts">
import { computed, onMounted, onUnmounted, watch, ref } from "vue";
import AgentSelector from "./components/AgentSelector.vue";
import ChatTabs from "./components/ChatTabs.vue";
import ChatWindow from "./components/ChatWindow.vue";
import Dashboard from "./components/Dashboard.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import ErrorBoundary from "./components/ErrorBoundary.vue";
import Navbar from "./components/Navbar.vue";
import ProvenancePane from "./components/ProvenancePane.vue";
import SettingsView from "./components/SettingsView.vue";
import ToastContainer from "./components/ToastContainer.vue";
import { useProvenanceOps } from "./composables/useProvenanceOps";
import { useChatTabs } from "./composables/useChatTabs";
import { useTheme } from "./composables/useTheme";
import { useConfirm } from "./composables/useConfirm";
import { parseMermaidBlocks } from "./utils/parseMermaid";
import type { AgentDiscoveryEntry, ChatMessage } from "./types/a2a";

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
const contextId = computed(() => activeClient.value?.contextId.value ?? undefined);
const taskId = computed(() => activeClient.value?.taskId.value ?? null);
const workflowProgress = computed(() => activeClient.value?.workflowProgress.value ?? { phase: "idle" as const, nodes: [], completedNodes: [] });
const awaitingInput = computed(() => activeClient.value?.awaitingInput.value ?? false);
const inputRequiredPrompt = computed(() => activeClient.value?.inputRequiredPrompt.value ?? "");

function fetchAgents() { activeClient.value?.fetchAgents(); }
function selectAgent(agent: AgentDiscoveryEntry) {
  activeClient.value?.selectAgent(agent);
  if (activeTabId.value) {
    renameTab(activeTabId.value, agent.agent_card?.name ?? agent.name);
  }
}
function sendMessage(text: string) { activeClient.value?.sendMessage(text); }
function cancelStream() { activeClient.value?.cancelStream(); }

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

// Active view — defaults to dashboard as landing page
const view = ref<"dashboard" | "chat" | "settings">("dashboard");

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
  fetchAgents();
  checkHealth();
  healthTimer = setInterval(checkHealth, 30_000);
});

onUnmounted(() => {
  if (healthTimer) clearInterval(healthTimer);
});
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
          :context-id="contextId"
          :context-metrics="contextMetrics"
          :provenance-diagram="provenanceDiagram"
          :messages="messages"
          :provenance-summary="provenanceDashboardSummary"
          @open-settings="view = 'settings'"
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
        </div>

        <div class="app-body">
          <ChatWindow
            :messages="messages"
            :is-loading="isLoading"
            :disabled="!selectedAgent"
            :awaiting-input="awaitingInput"
            :input-required-prompt="inputRequiredPrompt"
            :workflow-progress="workflowProgress"
            @send="sendMessage"
            @cancel="cancelStream"
          />
          <ProvenancePane
            :context-id="contextId"
            :task-id="taskId ?? undefined"
            :selected-agent-id="undefined"
            :is-streaming="isLoading"
            :diagrams="provenancePaneDiagrams"
            :trace-refresh-tick="traceRefreshGeneration"
          />
        </div>
      </div>
      </ErrorBoundary>

      <ErrorBoundary v-else-if="view === 'settings'">
        <SettingsView />
      </ErrorBoundary>
    </div>
    <ToastContainer />
    <ConfirmDialog />
  </div>
</template>
