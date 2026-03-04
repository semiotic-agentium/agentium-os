<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import AgentSelector from "./components/AgentSelector.vue";
import ChatWindow from "./components/ChatWindow.vue";
import Dashboard from "./components/Dashboard.vue";
import Navbar from "./components/Navbar.vue";
import ReasoningPane from "./components/ReasoningPane.vue";
import { useA2aClient } from "./composables/useA2aClient";
import { useTheme } from "./composables/useTheme";
import { parseMermaidBlocks } from "./utils/parseMermaid";

const {
  agents,
  selectedAgent,
  messages,
  isLoading,
  provenanceDiagram,
  contextMetrics,
  workflowProgress,
  awaitingInput,
  inputRequiredPrompt,
  fetchAgents,
  selectAgent,
  sendMessage,
} = useA2aClient();
const { theme, toggle: toggleTheme } = useTheme();

// Active view — defaults to dashboard as landing page
const view = ref<"dashboard" | "chat">("dashboard");

// Diagrams: provenance sequence diagram first, then any mermaid blocks in agent messages.
const diagrams = computed(() => {
  const inline = messages.value.flatMap((m) =>
    m.role === "agent" ? parseMermaidBlocks(m.text) : [],
  );
  return provenanceDiagram.value
    ? [provenanceDiagram.value, ...inline]
    : inline;
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
      <Dashboard v-show="view === 'dashboard'" :agents="agents" :context-metrics="contextMetrics" :provenance-diagram="provenanceDiagram" :messages="messages" />

      <div v-show="view === 'chat'" class="chat-layout">
        <div class="chat-toolbar">
          <AgentSelector :agents="agents" :selected="selectedAgent" @select="selectAgent" />
        </div>

        <div class="app-body">
          <ReasoningPane :diagrams="diagrams" />
          <ChatWindow
            :messages="messages"
            :is-loading="isLoading"
            :disabled="!selectedAgent"
            :awaiting-input="awaitingInput"
            :input-required-prompt="inputRequiredPrompt"
            :workflow-progress="workflowProgress"
            @send="sendMessage"
          />
        </div>
      </div>
    </div>
  </div>
</template>
