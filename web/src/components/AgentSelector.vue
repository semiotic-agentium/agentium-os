<script setup lang="ts">
import type { AgentDiscoveryEntry } from "../types/a2a";

const props = defineProps<{
  agents: AgentDiscoveryEntry[];
  selected: AgentDiscoveryEntry | null;
}>();

const emit = defineEmits<{
  select: [agent: AgentDiscoveryEntry];
}>();

function onChange(event: Event) {
  const idx = parseInt((event.target as HTMLSelectElement).value, 10);
  if (props.agents[idx]) {
    emit("select", props.agents[idx]);
  }
}
</script>

<template>
  <div class="agent-selector">
    <span v-if="agents.length > 0" class="status-dot"></span>
    <select :disabled="agents.length === 0" @change="onChange">
      <option v-if="agents.length === 0" disabled>No agents</option>
      <option
        v-for="(agent, idx) in agents"
        :key="agent.agent_package"
        :value="idx"
        :selected="selected?.agent_package === agent.agent_package"
      >
        {{ agent.name }} v{{ agent.version }}
      </option>
    </select>
  </div>
</template>
