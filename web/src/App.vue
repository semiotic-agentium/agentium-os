<script setup lang="ts">
import { computed, onMounted, watch, ref } from "vue";
import AgentSelector from "./components/AgentSelector.vue";
import ChatWindow from "./components/ChatWindow.vue";
import Dashboard from "./components/Dashboard.vue";
import Navbar from "./components/Navbar.vue";
import ProvenancePane from "./components/ProvenancePane.vue";
import SettingsView from "./components/SettingsView.vue";
import { useProvenanceOps } from "./composables/useProvenanceOps";
import { useA2aClient } from "./composables/useA2aClient";
import { useTheme } from "./composables/useTheme";
import { parseMermaidBlocks } from "./utils/parseMermaid";
import type { AgentDiscoveryEntry } from "./types/a2a";

const {
  agents,
  selectedAgent,
  messages,
  isLoading,
  provenanceDiagram,
  contextMetrics,
  contextId,
  taskId,
  workflowProgress,
  awaitingInput,
  inputRequiredPrompt,
  fetchAgents,
  selectAgent,
  sendMessage,
  cancelStream,
} = useA2aClient();

function handleSelectAgent(agent: AgentDiscoveryEntry): void {
  if (
    messages.value.length > 0 &&
    agent.agent_package !== selectedAgent.value?.agent_package
  ) {
    if (!window.confirm("Switching agents will clear the current conversation. Continue?")) return;
  }
  selectAgent(agent);
}
const { theme, toggle: toggleTheme } = useTheme();
const { createQuery } = useProvenanceOps();

// Active view — defaults to dashboard as landing page
const view = ref<"dashboard" | "chat" | "settings">("dashboard");

// Diagrams: provenance sequence diagram first, then any mermaid blocks in agent messages.
const diagrams = computed(() => {
  const inline = messages.value.flatMap((m) =>
    m.role === "agent" ? parseMermaidBlocks(m.text) : [],
  );
  return provenanceDiagram.value
    ? [provenanceDiagram.value, ...inline]
    : inline;
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

onMounted(() => fetchAgents());
</script>

<template>
  <div class="app">
    <Navbar
      :view="view"
      :agent-count="agents.length"
      :theme="theme"
      @change-view="view = $event"
      @toggle-theme="toggleTheme"
    />

    <div class="app-content-area">
      <Dashboard
        v-if="view === 'dashboard'"
        :agents="agents"
        :context-metrics="contextMetrics"
        :provenance-diagram="provenanceDiagram"
        :messages="messages"
        :provenance-summary="provenanceDashboardSummary"
        @open-settings="view = 'settings'"
      />

      <div v-else-if="view === 'chat'" class="chat-layout">
        <div class="chat-toolbar">
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
            :diagrams="diagrams"
          />
        </div>
      </div>

      <SettingsView v-else-if="view === 'settings'" />
    </div>
  </div>
</template>
