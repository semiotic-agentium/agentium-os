<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
defineProps<{
  displayName: string;
  status: string;
  ordinal?: number;
  hasExplicitOrdinal?: boolean;
}>();
</script>

<template>
  <div class="tool-card-header">
    <span class="tool-name">
      {{ displayName }}
      <span v-if="hasExplicitOrdinal" class="tool-ordinal">#{{ ordinal }}</span>
    </span>
    <span class="tool-status">{{ status }}</span>
  </div>
</template>

<style scoped>
.tool-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.5rem 0.75rem;
  font-size: 0.8125rem;
  border-bottom: 1px solid var(--border-subtle);
  background: var(--bg-subtle);
}

.tool-name {
  font-weight: 600;
  color: var(--text);
}

.tool-ordinal {
  margin-left: 0.35rem;
  font-size: 0.72rem;
  color: var(--text-muted);
  font-variant-numeric: tabular-nums;
}

.tool-status {
  color: var(--text-muted);
}
</style>
