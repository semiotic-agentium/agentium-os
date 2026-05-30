<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
export type LabeledPreVariant = "read" | "step" | "comms" | "failure";

defineProps<{
  label: string;
  text: string;
  variant: LabeledPreVariant;
}>();
</script>

<template>
  <div class="labeled-pre" :data-variant="variant">
    <div class="labeled-pre__label">{{ label }}</div>
    <pre class="labeled-pre__body">{{ text }}</pre>
  </div>
</template>

<style scoped>
.labeled-pre {
  padding: 0.45rem 0.5rem;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border-subtle);
}

.labeled-pre[data-variant="read"] {
  background: color-mix(in srgb, var(--surface) 80%, var(--bg-subtle));
}

.labeled-pre[data-variant="step"] {
  background: color-mix(in srgb, var(--bg-subtle) 72%, var(--surface));
}

.labeled-pre[data-variant="comms"] {
  background: color-mix(in srgb, var(--primary-subtle) 26%, var(--surface));
  border-color: color-mix(in srgb, var(--primary) 28%, var(--border-subtle));
}

.labeled-pre[data-variant="failure"] {
  background: color-mix(in srgb, var(--color-error) 10%, var(--surface));
  border-color: color-mix(in srgb, var(--color-error) 45%, var(--border-subtle));
}

.labeled-pre__label {
  font-size: 0.7rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  margin-bottom: 0.25rem;
}

.labeled-pre[data-variant="read"] .labeled-pre__label,
.labeled-pre[data-variant="step"] .labeled-pre__label {
  color: var(--text-muted);
}

.labeled-pre[data-variant="comms"] .labeled-pre__label {
  color: var(--primary);
}

.labeled-pre[data-variant="failure"] .labeled-pre__label {
  color: var(--color-error);
}

.labeled-pre__body {
  margin: 0;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  line-height: 1.35;
  white-space: pre-wrap;
  word-break: break-word;
}
</style>
