<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed } from "vue";
import type { AgentDiscoveryEntry } from "../types/a2a";

const props = withDefaults(
  defineProps<{
    variant: "chat" | "event";
    agents: AgentDiscoveryEntry[];
    subscribedAgents?: AgentDiscoveryEntry[];
    selected: AgentDiscoveryEntry | null;
    loading?: boolean;
  }>(),
  { subscribedAgents: () => [], loading: false },
);

const emit = defineEmits<{
  select: [agent: AgentDiscoveryEntry];
}>();

const label = computed(() =>
  props.variant === "chat" ? "Chat agent" : "Compose agent",
);

const selectId = computed(() =>
  props.variant === "chat" ? "operator-shell-chat-agent" : "operator-shell-event-agent",
);

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
  const target = event.target as HTMLSelectElement;
  if (props.variant === "chat") {
    const idx = parseInt(target.value, 10);
    const agent = props.agents[idx];
    if (agent) emit("select", agent);
    return;
  }
  const key = target.value;
  if (!key) return;
  const agent = props.agents.find((a) => agentKey(a) === key);
  if (agent) emit("select", agent);
}
</script>

<template>
  <div class="operator-agent-shell agent-selector">
    <label class="operator-agent-shell__label" :for="selectId">{{ label }}</label>
    <span v-if="variant === 'chat' && agents.length > 0" class="status-dot" aria-hidden="true" />
    <select
      v-if="variant === 'chat'"
      :id="selectId"
      name="operator-chat-agent"
      :disabled="loading || agents.length === 0"
      @change="onChange"
    >
      <option v-if="agents.length === 0" disabled value="">
        {{ loading ? "Loading agents…" : "No agents" }}
      </option>
      <option
        v-for="(agent, idx) in agents"
        :key="agentKey(agent)"
        :value="idx"
        :selected="selected?.agent_package === agent.agent_package &&
          selected?.agent_instance_id === agent.agent_instance_id"
      >
        {{ agent.name }} v{{ agent.version }}
      </option>
    </select>
    <select
      v-else
      :id="selectId"
      name="operator-event-agent"
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
.operator-agent-shell {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  flex: 1;
  min-width: 10rem;
  max-width: 28rem;
}

.operator-agent-shell__label {
  font-size: 0.6875rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  color: var(--text-muted);
  white-space: nowrap;
}

.operator-agent-shell select {
  flex: 1;
  min-width: 0;
  max-width: none;
  font-size: 12px;
  padding: 2px 4px;
}

.operator-agent-shell select:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}
</style>
