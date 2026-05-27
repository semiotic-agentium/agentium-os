<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { ref, computed, nextTick, watch } from "vue";
import type {
  AgentDiscoveryEntry,
  ChatMessage,
  HistoryHydrateState,
  WorkflowProgressState,
} from "../types/a2a";
import TranscriptView from "./TranscriptView.vue";
import WorkflowProgress from "./WorkflowProgress.vue";

const props = withDefaults(
  defineProps<{
    messages: ChatMessage[];
    isLoading: boolean;
    disabled: boolean;
    awaitingInput?: boolean;
    inputRequiredPrompt?: string;
    workflowProgress?: WorkflowProgressState;
    historyHydrateState?: HistoryHydrateState;
    selectedContextId?: string | null;
    agents?: AgentDiscoveryEntry[];
  }>(),
  { agents: () => [] },
);

const emit = defineEmits<{
  send: [text: string];
  cancel: [];
  "open-settings": [];
}>();

const input = ref("");
const chatWindowEl = ref<HTMLElement>();
const transcriptRef = ref<InstanceType<typeof TranscriptView> | null>(null);
const inputBarEl = ref<HTMLElement>();
const textarea = ref<HTMLTextAreaElement>();

function handleSend() {
  if (input.value.trim() && !streamBusy.value) {
    emit("send", input.value);
    input.value = "";
    if (textarea.value) {
      textarea.value.style.height = "auto";
    }
  }
}

const streamBusy = computed(() => props.isLoading && !props.awaitingInput);

const inputPlaceholder = computed(() => {
  if (props.awaitingInput) {
    const p = props.inputRequiredPrompt?.trim();
    return p && p.length > 0 ? p : "Type your reply…";
  }
  if (props.disabled && !props.isLoading) return "Select an agent to start";
  if (streamBusy.value) return "Agent is responding…";
  return "Type a message…";
});

function handleKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    handleSend();
  }
}

function autoGrow() {
  const el = textarea.value;
  if (el) {
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }
}

watch(
  () => props.awaitingInput,
  (on) => {
    if (on) {
      void nextTick(() => {
        textarea.value?.focus({ preventScroll: false });
      });
    }
  },
  { immediate: true },
);

watch(
  () => props.messages,
  async (messages) => {
    if (window.location.hostname !== "localhost") return;
    await nextTick();
    const container = transcriptRef.value?.getScrollContainer() ?? null;
    console.debug(
      "[transcript]",
      JSON.stringify({
        chatWindowCount: messages.length,
        sample: messages.slice(0, 4).map((m) => ({
          id: m.id,
          role: m.role,
          text: (m.text ?? "").slice(0, 60),
        })),
        container: container
          ? {
              childElementCount: container.childElementCount,
              clientHeight: container.clientHeight,
              scrollHeight: container.scrollHeight,
            }
          : null,
        chatWindow: chatWindowEl.value
          ? { clientHeight: chatWindowEl.value.clientHeight }
          : null,
        inputBar: inputBarEl.value ? { clientHeight: inputBarEl.value.clientHeight } : null,
        viewportHeight: window.innerHeight,
      }),
    );
  },
  { immediate: true, deep: true },
);
</script>

<template>
  <div ref="chatWindowEl" class="chat-window">
    <WorkflowProgress
      v-if="
        workflowProgress && workflowProgress.phase !== 'idle' && workflowProgress.pipelineActive
      "
      :progress="workflowProgress"
    />
    <TranscriptView
      ref="transcriptRef"
      variant="chat"
      :messages="messages"
      :hydrate-state="historyHydrateState ?? 'idle'"
      :disabled="disabled"
      :agents="agents"
      :selected-context-id="selectedContextId"
      @open-settings="emit('open-settings')"
    />
    <div v-if="streamBusy" class="working-indicator" role="status" aria-live="polite">
      <span class="working-dots" aria-hidden="true"><span></span><span></span><span></span></span>
      <span class="working-text">Agent is responding…</span>
    </div>
    <form ref="inputBarEl" class="input-bar" @submit.prevent="handleSend">
      <div
        v-if="awaitingInput"
        id="reply-needed-strip"
        class="reply-needed-strip"
        role="status"
        aria-live="polite"
      >
        <span class="reply-needed-strip__icon" aria-hidden="true">
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
            <path d="M8 10h.01" />
            <path d="M12 10h.01" />
            <path d="M16 10h.01" />
          </svg>
        </span>
        <span class="reply-needed-strip__text">Your reply is needed</span>
      </div>
      <div :class="['input-wrapper', { 'input-wrapper--awaiting': awaitingInput }]">
        <textarea
          ref="textarea"
          v-model="input"
          data-testid="message-input"
          rows="1"
          :placeholder="inputPlaceholder"
          :disabled="disabled || streamBusy"
          :aria-describedby="awaitingInput ? 'reply-needed-strip' : undefined"
          autofocus
          @keydown="handleKeydown"
          @input="autoGrow"
        ></textarea>
        <span class="input-hint">Enter to send, Shift+Enter for newline</span>
      </div>
      <button
        v-if="streamBusy"
        type="button"
        class="stop-btn"
        data-testid="stop-button"
        @click="emit('cancel')"
      >
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
          <rect x="5" y="5" width="14" height="14" rx="2" />
        </svg>
      </button>
      <button
        v-else
        type="submit"
        class="send-btn"
        data-testid="send-button"
        :disabled="disabled || streamBusy || !input.trim()"
      >
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <line x1="22" y1="2" x2="11" y2="13" />
          <polygon points="22 2 15 22 11 13 2 9 22 2" />
        </svg>
      </button>
    </form>
  </div>
</template>
