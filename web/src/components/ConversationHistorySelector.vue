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

function shortContextId(contextId: string): string {
  if (contextId.length <= 22) return contextId;
  return `${contextId.slice(0, 10)}…${contextId.slice(-6)}`;
}

function optionLabel(history: {
  contextId: string;
  latestTimestampMs: number;
  preview: string;
}): string {
  const preview = (history.preview ?? "").trim() || "(no preview)";
  const ts = formatTimestamp(history.latestTimestampMs);
  const id = shortContextId(history.contextId);
  return `${preview} · ${ts} · ${id}`;
}
</script>

<template>
  <div class="conversation-history-selector">
    <span class="history-label">Context</span>
    <select
      :disabled="disabled || loading || histories.length === 0"
      :value="selectedValue"
      @change="onChange"
    >
      <option value="" disabled>
        {{
          loading
            ? "Loading history..."
            : histories.length === 0
              ? "No prior conversations"
              : "Select context"
        }}
      </option>
      <option
        v-for="history in histories"
        :key="history.contextId"
        :value="history.contextId"
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
