<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { ref, onErrorCaptured } from "vue";

const hasError = ref(false);
const errorMessage = ref("");

onErrorCaptured((err) => {
  hasError.value = true;
  errorMessage.value = err instanceof Error ? err.message : String(err);
  console.error("[ErrorBoundary]", err);
  return false;
});

function retry() {
  hasError.value = false;
  errorMessage.value = "";
}
</script>

<template>
  <div v-if="hasError" class="error-boundary">
    <div class="error-boundary-content">
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="error-boundary-icon"
      >
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
      <h3>Something went wrong</h3>
      <p class="error-boundary-message">{{ errorMessage }}</p>
      <button class="btn btn--primary" @click="retry">Retry</button>
    </div>
  </div>
  <slot v-else />
</template>

<style scoped>
.error-boundary {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 200px;
  padding: 32px;
}

.error-boundary-content {
  text-align: center;
  max-width: 400px;
}

.error-boundary-icon {
  width: 48px;
  height: 48px;
  color: var(--color-error);
  margin-bottom: 16px;
}

.error-boundary-content h3 {
  font-size: var(--text-lg, 16px);
  margin-bottom: 8px;
  color: var(--text);
}

.error-boundary-message {
  font-size: var(--text-base, 13px);
  color: var(--text-muted);
  margin-bottom: 16px;
  word-break: break-word;
}
</style>
