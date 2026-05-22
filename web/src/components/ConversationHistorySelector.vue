<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, ref } from "vue";
import type { ConversationHistoryOption } from "../types/a2a";

const props = withDefaults(
  defineProps<{
    histories: ConversationHistoryOption[];
    selectedContextId: string | null;
    loading: boolean;
    disabled?: boolean;
    /** Event Console uses event-run copy; Chat uses conversation copy. */
    variant?: "chat" | "event";
  }>(),
  { variant: "chat" },
);

const selectRef = ref<HTMLSelectElement | null>(null);

const selectId = computed(() =>
  props.variant === "event" ? "event-console-event-run" : "chat-conversation-history",
);

const labelText = computed(() => (props.variant === "event" ? "Run" : "History"));

const emptyOptionText = computed(() => {
  if (props.loading) return "Loading…";
  if (props.histories.length === 0) {
    return props.variant === "event" ? "No event runs yet" : "No previous chats";
  }
  return props.variant === "event" ? "Pick an event run" : "Pick a previous chat";
});

const refreshAriaLabel = computed(() =>
  props.variant === "event" ? "Refresh event runs" : "Refresh conversation history",
);

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

function focusSelect(): void {
  selectRef.value?.focus();
}

defineExpose({ focusSelect });
</script>

<template>
  <div class="conversation-history-selector">
    <label class="history-label" :for="selectId">{{ labelText }}</label>
    <select
      :id="selectId"
      ref="selectRef"
      :name="variant === 'event' ? 'event-run' : 'conversation-history'"
      :disabled="disabled || loading || histories.length === 0"
      :value="selectedValue"
      @change="onChange"
    >
      <option value="" disabled>{{ emptyOptionText }}</option>
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
      :title="refreshAriaLabel"
      :aria-label="refreshAriaLabel"
      @click="emit('refresh')"
    >
      <span aria-hidden="true">↻</span>
    </button>
  </div>
</template>
