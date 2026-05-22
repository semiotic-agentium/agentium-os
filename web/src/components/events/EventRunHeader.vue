<script setup lang="ts">
import { ref } from "vue";
import type { ConversationHistoryOption } from "../../types/a2a";
import ConversationHistorySelector from "../ConversationHistorySelector.vue";

defineProps<{
  histories: ConversationHistoryOption[];
  selectedContextId: string | null;
  historyLoading: boolean;
  historyFetchError: string | null;
  historyRunsHint: string | null;
}>();

const emit = defineEmits<{
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
  <header class="event-run-header" aria-label="Event run controls">
    <div class="run-header-main">
      <ConversationHistorySelector
        ref="historySelectorRef"
        class="run-header-picker"
        variant="event"
        :histories="histories"
        :selected-context-id="selectedContextId"
        :loading="historyLoading"
        :disabled="false"
        @select="emit('select-context', $event)"
        @refresh="emit('refresh-history')"
      />

      <button type="button" class="btn btn--primary run-header-cta" @click="emit('new-event')">
        New event
      </button>
    </div>

    <p v-if="historyFetchError" class="run-header-hint run-header-hint--warn">
      {{ historyFetchError }}
    </p>
    <p v-else-if="historyRunsHint" class="run-header-hint">{{ historyRunsHint }}</p>
  </header>
</template>

<style scoped>
.event-run-header {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  padding: 0.5rem 1rem;
  border-bottom: 1px solid var(--border);
  flex-shrink: 0;
  background: var(--surface);
}

.run-header-main {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.5rem 0.65rem;
  min-height: 2.5rem;
}

.run-header-picker {
  flex: 1;
  min-width: 14rem;
  max-width: 28rem;
}

.run-header-cta {
  margin-left: auto;
  flex-shrink: 0;
}

@media (max-width: 720px) {
  .run-header-cta {
    margin-left: 0;
    width: 100%;
  }
}

.run-header-hint {
  margin: 0;
  font-size: 0.75rem;
  color: var(--text-muted);
}

.run-header-hint--warn {
  color: var(--color-error);
}
</style>
