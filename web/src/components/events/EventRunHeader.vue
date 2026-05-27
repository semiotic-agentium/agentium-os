<script setup lang="ts">
import { ref } from "vue";
import type { AgentDiscoveryEntry, ConversationHistoryOption } from "../../types/a2a";
import ConversationHistorySelector from "../ConversationHistorySelector.vue";
import OperatorAgentSelector from "../OperatorAgentSelector.vue";

defineProps<{
  agents: AgentDiscoveryEntry[];
  subscribedAgents: AgentDiscoveryEntry[];
  selectedAgent: AgentDiscoveryEntry | null;
  agentsLoading: boolean;
  histories: ConversationHistoryOption[];
  selectedContextId: string | null;
  historyLoading: boolean;
  historyFetchError: string | null;
  historyRunsHint: string | null;
}>();

const emit = defineEmits<{
  "select-agent": [agent: AgentDiscoveryEntry];
  "select-context": [option: ConversationHistoryOption];
  "refresh-history": [];
  "new-event": [];
}>();

const historySelectorRef = ref<InstanceType<typeof ConversationHistorySelector> | null>(null);

function focusEventRunSelect(): void {
  historySelectorRef.value?.focusSelect();
}

defineExpose({ focusEventRunSelect });
</script>

<template>
  <header class="event-run-toolbar" aria-label="Event run controls">
    <div class="chat-toolbar event-run-toolbar__row">
      <OperatorAgentSelector
        variant="event"
        class="chat-toolbar__agent event-run-toolbar__agent"
        :agents="agents"
        :subscribed-agents="subscribedAgents"
        :selected="selectedAgent"
        :loading="agentsLoading"
        @select="emit('select-agent', $event)"
      />

      <ConversationHistorySelector
        ref="historySelectorRef"
        class="event-run-toolbar__run"
        variant="event"
        :histories="histories"
        :selected-context-id="selectedContextId"
        :loading="historyLoading"
        :disabled="false"
        @select="emit('select-context', $event)"
        @refresh="emit('refresh-history')"
      />

      <button type="button" class="btn btn--primary event-run-toolbar__cta" @click="emit('new-event')">
        New event
      </button>
    </div>

    <p v-if="historyFetchError" class="event-run-toolbar__hint event-run-toolbar__hint--warn">
      {{ historyFetchError }}
    </p>
    <p v-else-if="historyRunsHint" class="event-run-toolbar__hint">{{ historyRunsHint }}</p>
  </header>
</template>

<style scoped>
.event-run-toolbar {
  display: flex;
  flex-direction: column;
  flex-shrink: 0;
  background: var(--surface);
}

.event-run-toolbar__row {
  flex-wrap: nowrap;
}

.event-run-toolbar__agent {
  max-width: 18rem;
}

.event-run-toolbar__run {
  flex: 1 1 12rem;
  min-width: 0;
  max-width: none;
}

.event-run-toolbar__cta {
  flex-shrink: 0;
}

@media (max-width: 960px) {
  .event-run-toolbar__row {
    flex-wrap: wrap;
  }

  .event-run-toolbar__run {
    flex: 1 1 100%;
    max-width: none;
  }

  .event-run-toolbar__cta {
    margin-left: auto;
  }
}

@media (max-width: 720px) {
  .event-run-toolbar__cta {
    margin-left: 0;
    width: 100%;
  }
}

.event-run-toolbar__hint {
  margin: 0;
  padding: 0 12px 4px;
  font-size: 0.75rem;
  line-height: 1.35;
  color: var(--text-muted);
}

.event-run-toolbar__hint--warn {
  color: var(--color-error);
}
</style>
