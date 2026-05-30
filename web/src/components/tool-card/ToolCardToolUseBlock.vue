<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
defineProps<{
  name: string;
  detail?: string;
}>();
</script>

<template>
  <div class="tool-use">
    <div class="tool-use-name">{{ name }}</div>
    <div v-if="detail" class="tool-use-detail">{{ detail }}</div>
  </div>
</template>

<style scoped>
.tool-use {
  padding: 0.35rem 0.5rem;
  border-radius: var(--radius-sm);
  background: var(--primary-subtle);
  border: 1px solid var(--border);
}

.tool-use-name {
  font-weight: 600;
  color: var(--primary);
  font-size: 0.8125rem;
}

.tool-use-detail {
  font-size: 0.75rem;
  color: var(--text-secondary);
  margin-top: 0.2rem;
  white-space: pre-wrap;
  word-break: break-word;
}
</style>
