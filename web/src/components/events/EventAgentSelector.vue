<script setup lang="ts">
import { computed } from "vue";
import type { AgentDiscoveryEntry } from "../../types/a2a";

const props = withDefaults(
  defineProps<{
    agents: AgentDiscoveryEntry[];
    subscribedAgents: AgentDiscoveryEntry[];
    selected: AgentDiscoveryEntry | null;
    loading?: boolean;
    /** Field label (modal uses "Agent"). */
    label?: string;
    selectId?: string;
  }>(),
  { label: "Deliver to", selectId: "event-console-deliver-agent" },
);

const emit = defineEmits<{
  select: [agent: AgentDiscoveryEntry];
}>();

function agentKey(agent: AgentDiscoveryEntry): string {
  return `${agent.agent_package}/${agent.agent_instance_id}`;
}

const selectedKey = computed(() =>
  props.selected ? agentKey(props.selected) : "",
);

const chatOnlyAgents = computed(() =>
  props.agents.filter((a) => (a.agent_card.subscriptions?.length ?? 0) === 0),
);

function onChange(event: Event): void {
  const key = (event.target as HTMLSelectElement).value;
  if (!key) return;
  const agent = props.agents.find((a) => agentKey(a) === key);
  if (agent) emit("select", agent);
}
</script>

<template>
  <div class="event-agent-selector agent-selector">
    <label class="event-agent-selector-label" :for="props.selectId">{{ props.label }}</label>
    <select
      :id="props.selectId"
      name="event-deliver-agent"
      :disabled="loading || agents.length === 0"
      :value="selectedKey"
      @change="onChange"
    >
      <option value="" disabled>
        {{
          loading
            ? "Loading agents…"
            : subscribedAgents.length === 0
              ? "No subscription agents"
              : "Select agent…"
        }}
      </option>
      <optgroup v-if="subscribedAgents.length > 0" label="Host dispatch">
        <option
          v-for="agent in subscribedAgents"
          :key="agentKey(agent)"
          :value="agentKey(agent)"
        >
          {{ agent.agent_package }}/{{ agent.agent_instance_id }}
        </option>
      </optgroup>
      <optgroup v-if="chatOnlyAgents.length > 0" label="Chat only (no subscriptions)">
        <option
          v-for="agent in chatOnlyAgents"
          :key="agentKey(agent)"
          :value="agentKey(agent)"
        >
          {{ agent.agent_package }}/{{ agent.agent_instance_id }}
        </option>
      </optgroup>
    </select>
  </div>
</template>

<style scoped>
.event-agent-selector {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex-shrink: 0;
}

.event-agent-selector-label {
  font-size: 0.75rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  color: var(--text-muted);
  white-space: nowrap;
}

.event-agent-selector select {
  min-width: 11rem;
  max-width: 18rem;
}

.event-agent-selector select:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}
</style>
