<script setup lang="ts">
import { computed } from "vue";
import type { ConversationHistoryOption } from "../types/a2a";

const props = defineProps<{
  histories: ConversationHistoryOption[];
  selectedContextId: string | null;
  loading: boolean;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  select: [option: ConversationHistoryOption];
  refresh: [];
}>();

const selectedValue = computed(() => props.selectedContextId ?? "");

function onChange(event: Event) {
  const contextId = (event.target as HTMLSelectElement).value;
  const option = props.histories.find((h) => h.contextId === contextId);
  if (option) {
    emit("select", option);
  }
}

function formatTimestamp(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function optionLabel(history: {
  contextId: string;
  latestTimestampMs: number;
  preview: string;
}): string {
  const preview = (history.preview ?? "").trim() || "(no preview)";
  const ts = formatTimestamp(history.latestTimestampMs);
  return ts ? `${preview} · ${ts}` : preview;
}
</script>

<template>
  <div class="conversation-history-selector">
    <span class="history-label">History</span>
    <select
      :disabled="disabled || loading || histories.length === 0"
      :value="selectedValue"
      @change="onChange"
    >
      <option value="" disabled>
        {{
          loading
            ? "Loading..."
            : histories.length === 0
              ? "No previous chats"
              : "Pick a previous chat"
        }}
      </option>
      <option
        v-for="history in histories"
        :key="history.contextId"
        :value="history.contextId"
        :title="history.contextId"
      >
        {{ optionLabel(history) }}
      </option>
    </select>
    <button
      type="button"
      class="history-refresh-btn"
      :disabled="disabled || loading"
      title="Refresh conversation history"
      @click="emit('refresh')"
    >
      ↻
    </button>
  </div>
</template>
