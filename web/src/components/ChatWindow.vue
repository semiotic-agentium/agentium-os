<script setup lang="ts">
import { ref, computed, nextTick, watch } from "vue";
import type {
  AgentDiscoveryEntry,
  ChatMessage,
  HistoryHydrateState,
  WorkflowProgressState,
} from "../types/a2a";
import MessageBubble from "./MessageBubble.vue";
import WorkflowProgress from "./WorkflowProgress.vue";

const props = withDefaults(
  defineProps<{
    messages: ChatMessage[];
    isLoading: boolean;
    disabled: boolean;
    /** Stream suspended: agent is waiting for user reply (TASK_STATE_INPUT_REQUIRED) */
    awaitingInput?: boolean;
    /** Optional prompt from agent (e.g. awaitInput(prompt)); show as hint/placeholder */
    inputRequiredPrompt?: string;
    /** Workflow progress state from coordinator SSE messages */
    workflowProgress?: WorkflowProgressState;
    /** Conversation-history GET / SSE restore (Primary empty states). */
    historyHydrateState?: HistoryHydrateState;
    /** Selected provenance context id (toolbar), for empty-state copy. */
    selectedContextId?: string | null;
    /** Available agents — used to render an empty-state picker when none is selected. */
    agents?: AgentDiscoveryEntry[];
  }>(),
  { agents: () => [] },
);

const emit = defineEmits<{
  send: [text: string];
  cancel: [];
  "select-agent": [agent: AgentDiscoveryEntry];
  "open-settings": [];
}>();

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
  if (input.value.trim() && !streamBusy.value) {
    userAtBottom.value = true;
    emit("send", input.value);
    input.value = "";
    if (textarea.value) {
      textarea.value.style.height = "auto";
    }
  }
}

/** Block input only while a stream is in flight, not when paused for INPUT_REQUIRED. */
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

const hydrateState = computed(() => props.historyHydrateState ?? "idle");

const emptyStateTitle = computed(() => {
  if (props.messages.length > 0) return "";
  if (props.disabled) return "Select an agent";
  const h = hydrateState.value;
  if (h === "loading") return "Loading transcript…";
  if (h === "error") return "Could not load history";
  if (h === "skipped") return "Live transcript in progress";
  if (h === "ready" && (props.selectedContextId ?? "").length > 0) {
    return "No messages in this context yet";
  }
  return "Send a message to start";
});

const emptyStateSubtitle = computed(() => {
  if (props.messages.length > 0) return "";
  const h = hydrateState.value;
  if (h === "error") {
    return "Check the runner /contexts API or try refreshing the context list.";
  }
  if (h === "skipped") {
    return "Pick up where you left off, or send a new message.";
  }
  if (h === "ready" && (props.selectedContextId ?? "").length > 0) {
    return "Say something to begin.";
  }
  return "Send a message to begin.";
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

// Auto-scroll to bottom when user is at bottom and content updates (messages, text, or tool/status events)
watch(
  () => {
    const last = props.messages[props.messages.length - 1];
    if (!last) return [props.messages.length];
    const eventCount =
      last.contentBlocks?.reduce((n, b) => n + (b.type === "tool" ? b.events.length : 0), 0) ?? 0;
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
    <WorkflowProgress
      v-if="
        workflowProgress && workflowProgress.phase !== 'idle' && workflowProgress.pipelineActive
      "
      :progress="workflowProgress"
    />
    <div
      ref="messagesContainer"
      class="messages"
      role="log"
      aria-live="polite"
      @scroll="onMessagesScroll"
    >
      <!-- Agent picker: shown when no agent is selected and transcript is empty -->
      <div
        v-if="messages.length === 0 && disabled && agents.length > 0"
        class="empty-state empty-state--picker"
      >
        <span class="empty-state-text">Pick an agent to start</span>
        <div class="agent-picker-grid">
          <button
            v-for="agent in agents"
            :key="agent.agent_package + '/' + agent.agent_instance_id"
            type="button"
            class="agent-picker-card"
            @click="emit('select-agent', agent)"
          >
            <span class="agent-picker-card__name">{{ agent.name }}</span>
            <span
              v-if="agent.agent_card.description"
              class="agent-picker-card__desc"
            >{{ agent.agent_card.description }}</span>
          </button>
        </div>
      </div>
      <!-- No agents deployed at all -->
      <div
        v-else-if="messages.length === 0 && disabled && agents.length === 0"
        class="empty-state"
      >
        <span class="empty-state-text">No agents deployed yet</span>
        <span class="empty-state-subtitle">
          Deploy one to start chatting.
        </span>
        <button type="button" class="empty-state-action" @click="emit('open-settings')">
          Open Settings → Deployments
        </button>
      </div>
      <!-- Default empty state: agent selected but no messages, or loading/error states -->
      <div v-else-if="messages.length === 0" class="empty-state">
        <!-- Chat icon -->
        <svg
          class="empty-state-icon"
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
        </svg>
        <span class="empty-state-text">{{ emptyStateTitle }}</span>
        <span v-if="emptyStateSubtitle" class="empty-state-subtitle">{{ emptyStateSubtitle }}</span>
      </div>
      <MessageBubble v-for="msg in messages" :key="msg.id" :message="msg" />
    </div>
    <div v-if="streamBusy" class="working-indicator" role="status" aria-live="polite">
      <span class="working-dots" aria-hidden="true"><span></span><span></span><span></span></span>
      <span class="working-text">Agent is responding…</span>
    </div>
    <form class="input-bar" @submit.prevent="handleSend">
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
        <!-- Send arrow icon -->
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
