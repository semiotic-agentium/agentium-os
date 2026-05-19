<script setup lang="ts">
import type { ConversationHistoryOption } from "../../types/a2a";

defineProps<{
  items: ConversationHistoryOption[];
  selectedContextId: string | null;
  loading: boolean;
  filterPreview: string;
  fetchError?: string | null;
}>();

const emit = defineEmits<{
  select: [string];
  refresh: [];
  "update:filterPreview": [string];
  useAsDraft: [];
}>();

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString();
}
</script>

<template>
  <div class="event-history-panel">
    <p class="history-intro field-hint">
      Provenance-backed contexts for the selected agent (same list as Chat). You can also use the
      <strong>Context</strong> dropdown in the toolbar while composing.
    </p>
    <p v-if="fetchError" class="field-hint field-hint--warn">{{ fetchError }}</p>
    <div class="history-filters">
      <input
        :value="filterPreview"
        type="search"
        placeholder="Filter by preview or context id"
        @input="emit('update:filterPreview', ($event.target as HTMLInputElement).value)"
      />
      <button type="button" class="btn btn--sm" :disabled="loading" @click="emit('refresh')">
        {{ loading ? "Loading…" : "Refresh" }}
      </button>
    </div>

    <ul class="history-list" role="list">
      <li
        v-for="item in items"
        :key="item.contextId"
        :class="['history-row', { active: selectedContextId === item.contextId }]"
        @click="emit('select', item.contextId)"
      >
        <div class="history-row-head">
          <span class="history-time">{{ formatTime(item.latestTimestampMs) }}</span>
        </div>
        <p class="history-preview">{{ item.preview }}</p>
        <code class="history-context-id">{{ item.contextId }}</code>
      </li>
      <li v-if="!loading && items.length === 0" class="history-empty">
        No contexts yet for this agent.
      </li>
    </ul>

    <div v-if="selectedContextId" class="history-actions">
      <button type="button" class="btn btn--sm" @click="emit('useAsDraft')">
        Continue in compose (bind scope)
      </button>
    </div>
  </div>
</template>

<style scoped>
.event-history-panel {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  min-height: 0;
}

.history-intro {
  margin: 0;
}

.history-filters {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.history-filters input {
  flex: 1;
  min-width: 8rem;
  font-size: 0.8125rem;
}

.history-list {
  list-style: none;
  margin: 0;
  padding: 0;
  overflow: auto;
  flex: 1;
  min-height: 0;
}

.history-row {
  padding: 0.65rem 0.75rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  margin-bottom: 0.5rem;
  cursor: pointer;
  background: var(--surface);
}

.history-row.active {
  border-color: var(--color-accent);
  background: var(--surface-raised);
}

.history-row-head {
  margin-bottom: 0.25rem;
}

.history-time {
  font-size: 0.75rem;
  color: var(--text-muted);
}

.history-preview {
  margin: 0 0 0.35rem;
  font-size: 0.8125rem;
}

.history-context-id {
  font-size: 0.7rem;
  word-break: break-all;
  color: var(--text-muted);
}

.history-empty {
  font-size: 0.8125rem;
  color: var(--text-muted);
  padding: 0.5rem;
}

.history-actions {
  border-top: 1px solid var(--border);
  padding-top: 0.75rem;
}
</style>
