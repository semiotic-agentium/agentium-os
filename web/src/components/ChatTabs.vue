<script setup lang="ts">
import type { ChatTab } from "../composables/useChatTabs";

defineProps<{
  tabs: ChatTab[];
  activeTabId: string | null;
}>();

const emit = defineEmits<{
  switch: [id: string];
  close: [id: string];
  create: [];
}>();

function onMousedown(e: MouseEvent, id: string) {
  // Middle-click closes the tab
  if (e.button === 1) {
    e.preventDefault();
    emit("close", id);
  }
}
</script>

<template>
  <div class="chat-tabs" role="tablist" aria-label="Chat tabs">
    <button
      v-for="tab in tabs"
      :key="tab.id"
      :class="['chat-tab', { active: tab.id === activeTabId }]"
      role="tab"
      :aria-selected="tab.id === activeTabId"
      :title="tab.title"
      @click="emit('switch', tab.id)"
      @mousedown="onMousedown($event, tab.id)"
    >
      <span
        v-if="tab.client.isLoading.value"
        class="chat-tab-status chat-tab-status--loading"
        aria-label="Loading"
      ></span>
      <span class="chat-tab-title">{{ tab.title }}</span>
      <button
        v-if="tabs.length > 1"
        class="chat-tab-close"
        title="Close tab"
        @click.stop="emit('close', tab.id)"
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
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
    </button>
    <button class="chat-tab-new" title="New chat tab" @click="emit('create')">
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <line x1="12" y1="5" x2="12" y2="19" />
        <line x1="5" y1="12" x2="19" y2="12" />
      </svg>
    </button>
  </div>
</template>
