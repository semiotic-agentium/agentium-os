<script setup lang="ts">
import { useConfirm } from "../composables/useConfirm";

const { state, handleResponse } = useConfirm();

function onOverlayClick(e: MouseEvent) {
  if (e.target === e.currentTarget) handleResponse(false);
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") handleResponse(false);
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="state.visible"
      class="confirm-overlay"
      role="dialog"
      aria-modal="true"
      :aria-label="state.title"
      @click="onOverlayClick"
      @keydown="onKeydown"
    >
      <div class="confirm-dialog">
        <h3 class="confirm-title">{{ state.title }}</h3>
        <p class="confirm-message">{{ state.message }}</p>
        <div class="confirm-actions">
          <button class="btn btn--secondary" @click="handleResponse(false)">Cancel</button>
          <button class="btn btn--primary" @click="handleResponse(true)" ref="confirmBtn">
            Confirm
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.confirm-overlay {
  position: fixed;
  inset: 0;
  z-index: 1000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.5);
}

.confirm-dialog {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg, 16px);
  padding: 24px;
  max-width: 420px;
  width: 90vw;
  box-shadow: var(--shadow-lg);
}

.confirm-title {
  font-size: var(--text-lg, 16px);
  margin-bottom: 8px;
  color: var(--text);
}

.confirm-message {
  font-size: var(--text-md, 14px);
  color: var(--text-secondary);
  margin-bottom: 20px;
  line-height: 1.5;
}

.confirm-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}
</style>
