<script setup lang="ts">
import type { ChatMessage, ContentBlock } from "../types/a2a";
import ToolNotificationCard from "./ToolNotificationCard.vue";

const props = withDefaults(
  defineProps<{ message: ChatMessage; showInlineStreamingDots?: boolean }>(),
  { showInlineStreamingDots: true },
);

function formatTime(date: Date): string {
  return new Date(date).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function isToolBlock(block: ContentBlock): block is import("../types/a2a").ToolNotificationBlock {
  return block.type === "tool";
}
</script>

<template>
  <div :class="['message-row', message.role]">
    <!-- Agent avatar -->
    <div v-if="message.role === 'agent'" class="message-avatar">
      <div class="avatar-icon">
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
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
            <div class="bubble-text">
              <template v-if="block.text">
                {{ block.text }}
                <span v-if="showInlineStreamingDots && message.isStreaming && idx === message.contentBlocks!.length - 1" class="thinking-dots inline">
                  <span /><span /><span />
                </span>
              </template>
              <template v-else-if="showInlineStreamingDots && message.isStreaming && idx === message.contentBlocks!.length - 1">
                <span class="thinking-dots"><span /><span /><span /></span>
              </template>
            </div>
          </div>
          <ToolNotificationCard v-else-if="isToolBlock(block)" :block="block" />
        </template>
        <div v-if="showInlineStreamingDots && message.isStreaming" class="streaming-dots-row">
          <span class="thinking-dots"><span /><span /><span /></span>
        </div>
        <div v-if="message.awaitingInput" class="awaiting-input-hint" role="status">
          <span class="awaiting-input-dot" aria-hidden="true" />
          Waiting for your response
        </div>
      </template>
      <!-- Legacy single-text content -->
      <template v-else>
        <div :class="['bubble', message.role]">
          <div class="bubble-text">
            <template v-if="showInlineStreamingDots && message.isStreaming && !message.text">
              <span class="thinking-dots">
                <span /><span /><span />
              </span>
            </template>
            <template v-else-if="message.text">
              {{ message.text }}
              <span v-if="showInlineStreamingDots && message.isStreaming" class="thinking-dots inline">
                <span /><span /><span />
              </span>
            </template>
            <template v-else>
              {{ message.text }}
            </template>
          </div>
          <div v-if="message.awaitingInput" class="awaiting-input-hint" role="status">
            <span class="awaiting-input-dot" aria-hidden="true" />
            Waiting for your response
          </div>
        </div>
      </template>
      <span class="message-time">{{ formatTime(props.message.timestamp) }}</span>
    </div>
  </div>
</template>
