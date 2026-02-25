<script setup lang="ts">
import { computed, onMounted } from "vue";
import AgentSelector from "./components/AgentSelector.vue";
import ChatWindow from "./components/ChatWindow.vue";
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
  fetchAgents,
  selectAgent,
  sendMessage,
} = useA2aClient();
const { theme, toggle: toggleTheme } = useTheme();

// Diagrams: provenance sequence diagram first, then any mermaid blocks embedded in agent messages.
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
    <header class="app-header">
      <h1>Agent Chat</h1>
      <div class="header-controls">
        <AgentSelector :agents="agents" :selected="selectedAgent" @select="selectAgent" />
        <button class="theme-toggle" @click="toggleTheme" :title="theme === 'light' ? 'Switch to dark mode' : 'Switch to light mode'">
          <!-- Sun icon (shown in dark mode) -->
          <svg v-if="theme === 'dark'" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="5" />
            <line x1="12" y1="1" x2="12" y2="3" />
            <line x1="12" y1="21" x2="12" y2="23" />
            <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
            <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
            <line x1="1" y1="12" x2="3" y2="12" />
            <line x1="21" y1="12" x2="23" y2="12" />
            <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
            <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
          </svg>
          <!-- Moon icon (shown in light mode) -->
          <svg v-else xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
          </svg>
        </button>
      </div>
    </header>

    <div class="app-body">
      <ReasoningPane :diagrams="diagrams" />
      <ChatWindow
        :messages="messages"
        :is-loading="isLoading"
        :disabled="!selectedAgent"
        @send="sendMessage"
      />
    </div>
  </div>
</template>
