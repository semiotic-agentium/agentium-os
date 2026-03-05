<script setup lang="ts">
import { ref, computed, nextTick, watch } from "vue";
import type { ChatMessage, WorkflowProgressState } from "../types/a2a";
import MessageBubble from "./MessageBubble.vue";
import WorkflowProgress from "./WorkflowProgress.vue";

const props = defineProps<{
  messages: ChatMessage[];
  isLoading: boolean;
  disabled: boolean;
  /** Stream suspended: agent is waiting for user reply (TASK_STATE_INPUT_REQUIRED) */
  awaitingInput?: boolean;
  /** Optional prompt from agent (e.g. awaitInput(prompt)); show as hint/placeholder */
  inputRequiredPrompt?: string;
  /** Workflow progress state from coordinator SSE messages */
  workflowProgress?: WorkflowProgressState;
}>();

const emit = defineEmits<{ send: [text: string] }>();

const input = ref("");
const messagesContainer = ref<HTMLElement>();
const textarea = ref<HTMLTextAreaElement>();
const userAtBottom = ref(true);
const scrollThreshold = 80;

function onMessagesScroll() {
  const el = messagesContainer.value;
  if (!el) return;
  const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < scrollThreshold;
  userAtBottom.value = nearBottom;
}

function handleSend() {
  if (input.value.trim() && !props.isLoading) {
    userAtBottom.value = true;
    emit("send", input.value);
    input.value = "";
    if (textarea.value) {
      textarea.value.style.height = "auto";
    }
  }
}

const inputPlaceholder = computed(() => {
  if (props.awaitingInput && props.inputRequiredPrompt?.trim()) {
    return props.inputRequiredPrompt.trim();
  }
  if (props.disabled && !props.isLoading) return "Select an agent to start";
  if (props.isLoading) return "Agent is responding…";
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

// Auto-scroll to bottom when user is at bottom and content updates (messages, text, or tool/status events)
watch(
  () => {
    const last = props.messages[props.messages.length - 1];
    if (!last) return [props.messages.length];
    const eventCount =
      last.contentBlocks?.reduce(
        (n, b) => n + (b.type === "tool" ? b.events.length : 0),
        0,
      ) ?? 0;
    return [props.messages.length, last.text, eventCount];
  },
  async () => {
    await nextTick();
    if (!userAtBottom.value || !messagesContainer.value) return;
    messagesContainer.value.scrollTo(0, messagesContainer.value.scrollHeight);
  },
);
</script>

<template>
  <div class="chat-window">
    <WorkflowProgress v-if="workflowProgress && workflowProgress.phase !== 'idle'" :progress="workflowProgress" />
    <div ref="messagesContainer" class="messages" @scroll="onMessagesScroll">
      <div v-if="messages.length === 0" class="empty-state">
        <!-- Chat icon -->
        <svg class="empty-state-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
        </svg>
        <span class="empty-state-text">Send a message to start chatting</span>
      </div>
      <MessageBubble v-for="msg in messages" :key="msg.id" :message="msg" />
    </div>
    <div v-if="isLoading" class="working-indicator" role="status" aria-live="polite">
      <span class="working-dots" aria-hidden="true"><span /><span /><span /></span>
      <span class="working-text">Agent is responding…</span>
    </div>
    <form class="input-bar" @submit.prevent="handleSend">
      <div class="input-wrapper">
        <textarea
          ref="textarea"
          data-testid="message-input"
          v-model="input"
          rows="1"
          :placeholder="inputPlaceholder"
          :disabled="disabled || isLoading"
          autofocus
          @keydown="handleKeydown"
          @input="autoGrow"
        />
        <span class="input-hint">Enter to send, Shift+Enter for newline</span>
      </div>
      <button type="submit" class="send-btn" data-testid="send-button" :disabled="disabled || isLoading || !input.trim()">
        <!-- Send arrow icon -->
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <line x1="22" y1="2" x2="11" y2="13" />
          <polygon points="22 2 15 22 11 13 2 9 22 2" />
        </svg>
      </button>
    </form>
  </div>
</template>
