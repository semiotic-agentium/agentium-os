<script setup lang="ts">
import { ref, nextTick, watch } from "vue";
import type { ChatMessage } from "../types/a2a";
import MessageBubble from "./MessageBubble.vue";

const props = defineProps<{
  messages: ChatMessage[];
  isLoading: boolean;
  disabled: boolean;
}>();

const emit = defineEmits<{ send: [text: string] }>();

const input = ref("");
const messagesContainer = ref<HTMLElement>();
const textarea = ref<HTMLTextAreaElement>();

function handleSend() {
  if (input.value.trim() && !props.isLoading) {
    emit("send", input.value);
    input.value = "";
    if (textarea.value) {
      textarea.value.style.height = "auto";
    }
  }
}

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

// Auto-scroll to bottom on new messages or streaming updates
watch(
  () => [props.messages.length, props.messages[props.messages.length - 1]?.text],
  async () => {
    await nextTick();
    messagesContainer.value?.scrollTo(0, messagesContainer.value.scrollHeight);
  },
);
</script>

<template>
  <div class="chat-window">
    <div ref="messagesContainer" class="messages">
      <div v-if="messages.length === 0" class="empty-state">
        <!-- Chat icon -->
        <svg class="empty-state-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
        </svg>
        <span class="empty-state-text">Send a message to start chatting</span>
      </div>
      <MessageBubble v-for="msg in messages" :key="msg.id" :message="msg" />
    </div>
    <form class="input-bar" @submit.prevent="handleSend">
      <div class="input-wrapper">
        <textarea
          ref="textarea"
          v-model="input"
          rows="1"
          placeholder="Type a message..."
          :disabled="disabled || isLoading"
          autofocus
          @keydown="handleKeydown"
          @input="autoGrow"
        />
        <span class="input-hint">Enter to send, Shift+Enter for newline</span>
      </div>
      <button type="submit" class="send-btn" :disabled="disabled || isLoading || !input.trim()">
        <!-- Send arrow icon -->
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <line x1="22" y1="2" x2="11" y2="13" />
          <polygon points="22 2 15 22 11 13 2 9 22 2" />
        </svg>
      </button>
    </form>
  </div>
</template>
