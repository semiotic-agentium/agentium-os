<script setup lang="ts">
import { computed, ref } from "vue";
import type { ChatMessage, ContentBlock, DataContentBlock } from "../types/a2a";
import TaskTimeline from "./TaskTimeline.vue";
import ToolNotificationCard from "./ToolNotificationCard.vue";
import {
  parseCoordinatorAnswer,
  safeHostname,
  isUrl,
  type ParsedCoordinatorAnswer,
} from "../utils/parseCoordinatorAnswer";
import { renderMarkdown } from "../utils/renderMarkdown";

const props = withDefaults(
  defineProps<{ message: ChatMessage; showInlineStreamingDots?: boolean }>(),
  { showInlineStreamingDots: true },
);

function formatTime(date: Date): string {
  const parsed = new Date(date);
  if (Number.isNaN(parsed.getTime())) return "";
  return parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function isToolBlock(block: ContentBlock): block is import("../types/a2a").ToolNotificationBlock {
  return block.type === "tool";
}

function isDataBlock(block: ContentBlock): block is DataContentBlock {
  return block.type === "data";
}

function formatStructuredBody(mediaType: string, data: unknown): string {
  if (data === null || data === undefined) return "";
  if (mediaType === "application/json" || mediaType.includes("json")) {
    try {
      return JSON.stringify(data, null, 2);
    } catch {
      return String(data);
    }
  }
  if (mediaType === "text/csv" || mediaType === "text/plain") {
    return typeof data === "string" ? data : JSON.stringify(data, null, 2);
  }
  try {
    return typeof data === "string" ? data : JSON.stringify(data, null, 2);
  } catch {
    return String(data);
  }
}

const citationRefs = computed((): string[] => {
  const raw = props.message.metadata?.citations;
  if (!Array.isArray(raw)) return [];
  return raw.filter((x): x is string => typeof x === "string");
});

// Coordinator metadata parsing (only for finished agent messages)
const coordinatorData = computed((): ParsedCoordinatorAnswer | null => {
  if (props.message.role !== "agent") return null;
  if (props.message.isStreaming) return null;
  if (!props.message.text) return null;
  return parseCoordinatorAnswer(props.message.text);
});

const confidenceClass = computed(() => {
  const c = coordinatorData.value?.confidence;
  if (c == null) return "";
  if (c >= 0.7) return "confidence-high";
  if (c >= 0.4) return "confidence-medium";
  return "confidence-low";
});

const showGoals = ref(false);
const showGaps = ref(false);
</script>

<template>
  <div :class="['message-row', message.role]">
    <!-- Agent avatar -->
    <div v-if="message.role === 'agent'" class="message-avatar">
      <div class="avatar-icon">
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <rect x="3" y="11" width="18" height="10" rx="2" />
          <circle cx="12" cy="5" r="2" />
          <path d="M12 7v4" />
          <line x1="8" y1="16" x2="8" y2="16" />
          <line x1="16" y1="16" x2="16" y2="16" />
        </svg>
      </div>
    </div>
    <div class="message-content">
      <!-- Block-based content (agent with tool notifications) -->
      <template v-if="message.role === 'agent' && message.contentBlocks?.length">
        <template v-for="(block, idx) in message.contentBlocks" :key="idx">
          <div v-if="block.type === 'text'" :class="['bubble', message.role]">
            <div :class="['bubble-text', message.role === 'agent' ? 'bubble-markdown' : '']">
              <template v-if="block.text">
                <!-- eslint-disable-next-line vue/no-v-html -->
                <div v-if="message.role === 'agent'" v-html="renderMarkdown(block.text)"></div>
                <template v-else>{{ block.text }}</template>
                <span
                  v-if="
                    showInlineStreamingDots &&
                      message.isStreaming &&
                      idx === message.contentBlocks!.length - 1
                  "
                  class="thinking-dots inline"
                >
                  <span></span><span></span><span></span>
                </span>
              </template>
              <template
                v-else-if="
                  showInlineStreamingDots &&
                    message.isStreaming &&
                    idx === message.contentBlocks!.length - 1
                "
              >
                <span class="thinking-dots"><span></span><span></span><span></span></span>
              </template>
            </div>
          </div>
          <ToolNotificationCard v-else-if="isToolBlock(block)" :block="block" />
          <div
            v-else-if="isDataBlock(block)"
            :class="['bubble', 'bubble-data', message.role]"
            role="region"
            :aria-label="`Structured data (${block.mediaType})`"
          >
            <div class="structured-data-label">{{ block.mediaType }}</div>
            <pre class="structured-data-pre">{{
              formatStructuredBody(block.mediaType, block.data)
            }}</pre>
          </div>
        </template>
        <div v-if="showInlineStreamingDots && message.isStreaming" class="streaming-dots-row">
          <span class="thinking-dots"><span></span><span></span><span></span></span>
        </div>
        <div v-if="message.awaitingInput" class="awaiting-input-hint" role="status">
          <span class="awaiting-input-dot" aria-hidden="true"></span>
          Waiting for your response
        </div>
      </template>
      <!-- Legacy single-text content -->
      <template v-else>
        <div :class="['bubble', message.role]">
          <div :class="['bubble-text', message.role === 'agent' ? 'bubble-markdown' : '']">
            <template v-if="showInlineStreamingDots && message.isStreaming && !message.text">
              <span class="thinking-dots"> <span></span><span></span><span></span> </span>
            </template>
            <template v-else-if="message.text">
              <!-- eslint-disable-next-line vue/no-v-html -->
              <div v-if="message.role === 'agent'" v-html="renderMarkdown(message.text)"></div>
              <template v-else>{{ message.text }}</template>
              <span
                v-if="showInlineStreamingDots && message.isStreaming"
                class="thinking-dots inline"
              >
                <span></span><span></span><span></span>
              </span>
            </template>
            <template v-else>
              {{ message.text }}
            </template>
          </div>
          <div v-if="message.awaitingInput" class="awaiting-input-hint" role="status">
            <span class="awaiting-input-dot" aria-hidden="true"></span>
            Waiting for your response
          </div>
        </div>
      </template>
      <!-- Citation chips — rendered for both structured and legacy text messages -->
      <div v-if="citationRefs.length" class="citation-chip-row" role="group" aria-label="Citations">
        <span class="citation-section-label">Citations</span>
        <div class="source-chips">
          <span v-for="(ref, i) in citationRefs" :key="i" class="source-chip">{{ ref }}</span>
        </div>
      </div>
      <!-- Coordinator metadata footer (parsed from structured text) -->
      <div v-if="coordinatorData" class="coordinator-meta">
        <div v-if="coordinatorData.confidence !== null" class="coordinator-confidence">
          <span :class="['confidence-badge', confidenceClass]">
            Confidence: {{ (coordinatorData.confidence * 100).toFixed(0) }}%
          </span>
        </div>

        <div v-if="coordinatorData.sources.length > 0" class="coordinator-sources">
          <span class="coordinator-section-label">Sources</span>
          <div class="source-chips">
            <template v-for="src in coordinatorData.sources" :key="src">
              <a
                v-if="isUrl(src)"
                :href="src"
                target="_blank"
                rel="noopener noreferrer"
                class="source-chip"
              >
                {{ safeHostname(src) }}
              </a>
              <span v-else class="source-chip">{{ src }}</span>
            </template>
          </div>
        </div>

        <div v-if="coordinatorData.actionableGoals.length > 0" class="coordinator-collapsible">
          <button class="collapsible-toggle" @click="showGoals = !showGoals">
            {{ showGoals ? "\u25BE" : "\u25B8" }} Goals ({{
              coordinatorData.actionableGoals.length
            }})
          </button>
          <ul v-if="showGoals" class="coordinator-list">
            <li v-for="(g, i) in coordinatorData.actionableGoals" :key="i">
              {{ g.goal }}
              <span v-if="g.owner" class="goal-meta">Owner: {{ g.owner }}</span>
              <span v-if="g.dueDate" class="goal-meta">Due: {{ g.dueDate }}</span>
            </li>
          </ul>
        </div>

        <div v-if="coordinatorData.gaps.length > 0" class="coordinator-collapsible">
          <button class="collapsible-toggle" @click="showGaps = !showGaps">
            {{ showGaps ? "\u25BE" : "\u25B8" }} Gaps ({{ coordinatorData.gaps.length }})
          </button>
          <ul v-if="showGaps" class="coordinator-list">
            <li v-for="(gap, i) in coordinatorData.gaps" :key="i">{{ gap }}</li>
          </ul>
        </div>

        <div v-if="coordinatorData.clarificationQuestion" class="coordinator-clarification">
          <span class="coordinator-section-label">Clarification needed</span>
          <p>{{ coordinatorData.clarificationQuestion }}</p>
        </div>
      </div>

      <TaskTimeline
        v-if="message.stateTransitions?.length"
        :transitions="message.stateTransitions"
      />
      <span class="message-time">{{ formatTime(props.message.timestamp) }}</span>
    </div>
  </div>
</template>

<style scoped>
.bubble-data {
  max-width: 100%;
}

.structured-data-label {
  font-size: 0.7rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  opacity: 0.75;
  margin-bottom: 0.35rem;
}

.structured-data-pre {
  margin: 0;
  padding: 0.75rem 1rem;
  max-height: 20rem;
  overflow: auto;
  font-family: var(--font-mono);
  font-size: 0.8rem;
  line-height: 1.45;
  border-radius: 8px;
  background: color-mix(in srgb, var(--surface-raised) 88%, transparent);
  border: 1px solid color-mix(in srgb, var(--border) 80%, transparent);
  white-space: pre-wrap;
  word-break: break-word;
}

.citation-chip-row {
  margin-top: 0.5rem;
  margin-bottom: 0.25rem;
}

.citation-section-label {
  display: block;
  font-size: 0.7rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  opacity: 0.75;
  margin-bottom: 0.35rem;
}
</style>
