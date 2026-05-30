<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
defineProps<{
  text: string;
  count?: number;
  active?: boolean;
}>();
</script>

<template>
  <div class="system-row" :class="{ 'system-row--active': active }">
    <span class="system-dot" aria-hidden="true"></span>
    <span class="system-label">{{ text }}</span>
    <span v-if="count && count > 1" class="system-count">×{{ count }}</span>
  </div>
</template>

<style scoped>
.system-row {
  display: flex;
  align-items: center;
  gap: 0.4rem;
}

.system-dot {
  flex-shrink: 0;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--text-muted);
  opacity: 0.5;
}

.system-row--active .system-dot {
  background: var(--primary, #6366f1);
  opacity: 1;
  animation: pulse-dot 1.4s ease-in-out infinite;
}

.system-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.system-count {
  flex-shrink: 0;
  font-size: 0.7rem;
  color: var(--text-muted);
  background: var(--bg-subtle);
  border: 1px solid var(--border-subtle);
  border-radius: 3px;
  padding: 0 0.3rem;
  font-variant-numeric: tabular-nums;
}

@keyframes pulse-dot {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.35;
  }
}
</style>
