<script setup lang="ts">
import type { ChatMessage } from "../types/a2a";

const props = defineProps<{ message: ChatMessage }>();

function formatTime(date: Date): string {
  return new Date(date).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
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
      <div :class="['bubble', message.role]">
        <div class="bubble-text">
          <template v-if="message.isStreaming && !message.text">
            <span class="thinking-dots">
              <span /><span /><span />
            </span>
          </template>
          <template v-else>
            {{ message.text }}
          </template>
        </div>
      </div>
      <span class="message-time">{{ formatTime(props.message.timestamp) }}</span>
    </div>
  </div>
</template>
