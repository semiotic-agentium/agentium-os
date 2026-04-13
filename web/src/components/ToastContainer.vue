<script setup lang="ts">
import { useToast } from "../composables/useToast";

const { toasts, dismiss } = useToast();
</script>

<template>
  <Teleport to="body">
    <div v-if="toasts.length" class="toast-container" aria-live="polite">
      <div
        v-for="toast in toasts"
        :key="toast.id"
        :class="['toast', `toast--${toast.type}`, { 'toast--leaving': toast.leaving }]"
        role="status"
        @click="dismiss(toast.id)"
      >
        <!-- Check icon for success -->
        <svg
          v-if="toast.type === 'success'"
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          style="width: 16px; height: 16px; color: var(--color-success); flex-shrink: 0"
        >
          <polyline points="20 6 9 17 4 12" />
        </svg>
        <!-- X icon for error -->
        <svg
          v-else-if="toast.type === 'error'"
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          style="width: 16px; height: 16px; color: var(--color-error); flex-shrink: 0"
        >
          <circle cx="12" cy="12" r="10" />
          <line x1="15" y1="9" x2="9" y2="15" />
          <line x1="9" y1="9" x2="15" y2="15" />
        </svg>
        <span>{{ toast.message }}</span>
      </div>
    </div>
  </Teleport>
</template>
