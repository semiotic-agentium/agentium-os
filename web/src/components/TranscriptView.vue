<script setup lang="ts">
import { ref, computed, nextTick, watch } from "vue";
import type { AgentDiscoveryEntry, ChatMessage, HistoryHydrateState } from "../types/a2a";
import type { TraceObserveState } from "../composables/useEventObservation";
import MessageBubble from "./MessageBubble.vue";

export type TranscriptHydrateState = HistoryHydrateState | TraceObserveState;

const props = withDefaults(
  defineProps<{
    messages: ChatMessage[];
    variant: "chat" | "event";
    hydrateState?: TranscriptHydrateState;
    isStreaming?: boolean;
    selectedContextId?: string | null;
    /** Chat: no agent selected */
    disabled?: boolean;
    agents?: AgentDiscoveryEntry[];
    /** Event: subscribers accepted but provenance ingress not yet visible */
    waitingForIngress?: boolean;
    /** Event: a publish completed for the current scope */
    hasPublishedRun?: boolean;
  }>(),
  {
    hydrateState: "idle",
    isStreaming: false,
    disabled: false,
    agents: () => [],
    waitingForIngress: false,
    hasPublishedRun: false,
  },
);

const emit = defineEmits<{
  "select-agent": [agent: AgentDiscoveryEntry];
  "open-settings": [];
  "compose-event": [];
  "focus-event-run": [];
}>();

const showEventOnboarding = computed(
  () =>
    props.variant === "event" &&
    props.messages.length === 0 &&
    !(props.selectedContextId ?? "").length &&
    hydrate.value !== "loading" &&
    hydrate.value !== "waiting" &&
    !props.waitingForIngress &&
    !(props.hasPublishedRun && hydrate.value === "empty"),
);

const messagesContainer = ref<HTMLElement>();
const userAtBottom = ref(true);
const scrollThreshold = 80;

function onMessagesScroll() {
  const el = messagesContainer.value;
  if (!el) return;
  const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < scrollThreshold;
  userAtBottom.value = nearBottom;
}

const hydrate = computed(() => props.hydrateState ?? "idle");

/** Context restore: scroll to top after bulk hydrate (chat + event history pick). */
watch(hydrate, async (next, prev) => {
  if (next === "loading") {
    userAtBottom.value = false;
  }
  if (prev === "loading" && next === "ready" && props.messages.length > 0) {
    await nextTick();
    const el = messagesContainer.value;
    if (el) el.scrollTop = 0;
  }
});

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

const emptyStateTitle = computed(() => {
  if (props.messages.length > 0) return "";
  if (props.variant === "chat") {
    if (props.disabled) return "Select an agent";
    const h = hydrate.value;
    if (h === "loading") return "Loading transcript…";
    if (h === "error") return "Could not load history";
    if (h === "skipped") return "Live transcript in progress";
    if (h === "ready" && (props.selectedContextId ?? "").length > 0) {
      return "No messages in this context yet";
    }
    return "Send a message to start";
  }
  const h = hydrate.value;
  if (h === "loading" || h === "waiting") return "Loading provenance transcript…";
  if (h === "error") return "Could not load transcript";
  if (props.waitingForIngress) return "Waiting for host ingress…";
  if (props.hasPublishedRun && h === "empty") {
    return "Subscribers accepted — transcript pending";
  }
  if ((props.selectedContextId ?? "").length > 0 && h === "ready") {
    return "No transcript rows for this context";
  }
  return "Observe a run";
});

const emptyStateSubtitle = computed(() => {
  if (props.messages.length > 0) return "";
  if (props.variant === "chat") {
    const h = hydrate.value;
    if (h === "error") {
      return "Check the runner /contexts API or try refreshing the context list.";
    }
    if (h === "skipped") return "Pick up where you left off, or send a new message.";
    if (h === "ready" && (props.selectedContextId ?? "").length > 0) {
      return "Say something to begin.";
    }
    return "Send a message to begin.";
  }
  const h = hydrate.value;
  if (h === "error") {
    return "Check the runner conversation-history API or refresh the context.";
  }
  if (props.waitingForIngress || (props.hasPublishedRun && h === "empty")) {
    return "Host ingress and agent steps appear when provenance catches up.";
  }
  if ((props.selectedContextId ?? "").length > 0) {
    return "Pick another context or compose a new event.";
  }
  return "Compose an event or pick a context from the toolbar.";
});

defineExpose({
  getScrollContainer: () => messagesContainer.value ?? null,
});
</script>

<template>
  <div class="transcript-view">
    <h2 v-if="variant === 'event'" id="event-console-transcript-heading" class="sr-only">
      Provenance transcript
    </h2>
    <div
      ref="messagesContainer"
      class="messages"
      role="log"
      aria-live="polite"
      :aria-labelledby="variant === 'event' ? 'event-console-transcript-heading' : undefined"
      :aria-label="variant === 'chat' ? 'Chat transcript' : undefined"
      @scroll="onMessagesScroll"
    >
      <template v-if="variant === 'chat'">
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
        <div
          v-else-if="messages.length === 0 && disabled && agents.length === 0"
          class="empty-state"
        >
          <span class="empty-state-text">No agents deployed yet</span>
          <span class="empty-state-subtitle">Deploy one to start chatting.</span>
          <button type="button" class="empty-state-action" @click="emit('open-settings')">
            Open Settings → Deployments
          </button>
        </div>
        <div v-else-if="messages.length === 0" class="empty-state">
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
      </template>

      <template v-else>
        <div
          v-if="messages.length === 0 && showEventOnboarding"
          class="empty-state empty-state--event-onboarding"
        >
          <h2 class="event-onboarding-title">Observe an Event Run</h2>
          <ol class="event-onboarding-steps">
            <li>Choose agent and message type in Configure</li>
            <li>Publish an event or pick a recent run</li>
            <li>Watch host ingress and agent steps in this transcript</li>
          </ol>
          <div class="event-onboarding-actions">
            <button type="button" class="empty-state-action empty-state-action--primary" @click="emit('compose-event')">
              New event
            </button>
            <button type="button" class="empty-state-action" @click="emit('focus-event-run')">
              Pick a run
            </button>
          </div>
        </div>
        <div v-else-if="messages.length === 0" class="empty-state">
          <svg
            class="empty-state-icon"
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H19a1 1 0 0 1 1 1v18a1 1 0 0 1-1 1H6.5a1 1 0 0 1-1-1Z" />
            <path d="M8 7h8" />
            <path d="M8 11h6" />
          </svg>
          <span class="empty-state-text">{{ emptyStateTitle }}</span>
          <span v-if="emptyStateSubtitle" class="empty-state-subtitle">{{ emptyStateSubtitle }}</span>
          <button
            v-if="!selectedContextId"
            type="button"
            class="empty-state-action"
            @click="emit('compose-event')"
          >
            Publish Event
          </button>
        </div>
      </template>

      <MessageBubble
        v-for="(msg, i) in messages"
        :key="msg.id ?? `msg-${i}`"
        :message="msg"
      />
    </div>

    <div
      v-if="variant === 'event' && isStreaming"
      class="working-indicator"
      role="status"
      aria-live="polite"
    >
      <span class="working-dots" aria-hidden="true"><span></span><span></span><span></span></span>
      <span class="working-text">Recording provenance…</span>
    </div>

    <slot name="footer" />
  </div>
</template>

<style scoped>
.transcript-view {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
  height: 100%;
  overflow: hidden;
}

.empty-state--event-onboarding {
  max-width: 28rem;
  padding: 1.5rem;
  margin: auto;
  text-align: left;
  align-items: flex-start;
}

.event-onboarding-title {
  margin: 0;
  font-size: 1.125rem;
  font-weight: 600;
  color: var(--text-secondary);
  text-wrap: balance;
}

.event-onboarding-steps {
  margin: 0;
  padding-left: 1.25rem;
  font-size: 0.875rem;
  line-height: 1.5;
  color: var(--text-muted);
}

.event-onboarding-steps li {
  margin-bottom: 0.35rem;
}

.event-onboarding-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  margin-top: 0.25rem;
}

.empty-state-action--primary {
  color: var(--primary);
  border-color: color-mix(in srgb, var(--primary) 35%, var(--border));
  background: var(--primary-subtle);
}
</style>
