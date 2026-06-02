<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed } from "vue";
import {
  fieldsFromSchema,
  getAtPath,
  setAtPath,
  type SchemaFormField,
} from "../../events/schemaForm";

const props = defineProps<{
  schema: unknown;
  model: Record<string, unknown>;
  labels?: Record<string, string>;
  focusPath?: string | null;
  depth?: number;
}>();

const emit = defineEmits<{
  update: [Record<string, unknown>];
}>();

const fields = computed(() => fieldsFromSchema(props.schema, props.labels));

function valueFor(field: SchemaFormField): unknown {
  return getAtPath(props.model, field.path);
}

function nestedModel(field: SchemaFormField): Record<string, unknown> {
  const value = valueFor(field);
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  return {};
}

function onInput(field: SchemaFormField, raw: string | boolean | number | unknown): void {
  const next = { ...props.model };
  setAtPath(next, field.path, raw);
  emit("update", next);
}

function onNestedUpdate(field: SchemaFormField, nested: Record<string, unknown>): void {
  onInput(field, nested);
}

function isFocused(field: SchemaFormField): boolean {
  return props.focusPath === field.path;
}

function constLabel(field: SchemaFormField): string {
  if (typeof field.constValue === "string") return field.constValue;
  return JSON.stringify(field.constValue ?? "");
}
</script>

<template>
  <div class="schema-form-fields" :class="{ 'schema-form-fields--nested': (depth ?? 0) > 0 }">
    <div
      v-for="field in fields"
      :key="field.path"
      :class="['schema-field', { 'schema-field--focused': isFocused(field) }]"
    >
      <label class="schema-field-label">
        {{ field.title }}
        <span v-if="field.required" class="required-mark">*</span>
      </label>

      <span v-if="field.kind === 'const'" class="const-pill">{{ constLabel(field) }}</span>

      <section
        v-else-if="field.kind === 'object' && field.objectSchema"
        class="schema-nested"
      >
        <EventSchemaFormFields
          :schema="field.objectSchema"
          :model="nestedModel(field)"
          :labels="labels"
          :focus-path="focusPath"
          :depth="(depth ?? 0) + 1"
          @update="(m) => onNestedUpdate(field, m)"
        />
      </section>

      <select
        v-else-if="field.kind === 'enum' && field.enumValues"
        :value="String(valueFor(field) ?? '')"
        @change="onInput(field, ($event.target as HTMLSelectElement).value)"
      >
        <option value="">—</option>
        <option v-for="opt in field.enumValues" :key="opt" :value="opt">{{ opt }}</option>
      </select>
      <input
        v-else-if="field.kind === 'boolean'"
        type="checkbox"
        :checked="Boolean(valueFor(field))"
        @change="onInput(field, ($event.target as HTMLInputElement).checked)"
      />
      <input
        v-else-if="field.kind === 'number' || field.kind === 'integer'"
        type="number"
        :value="Number(valueFor(field) ?? 0)"
        @input="onInput(field, Number(($event.target as HTMLInputElement).value))"
      />
      <textarea
        v-else-if="field.kind === 'array' || field.kind === 'unknown'"
        :value="
          typeof valueFor(field) === 'string'
            ? (valueFor(field) as string)
            : JSON.stringify(valueFor(field) ?? [], null, 2)
        "
        rows="4"
        @input="
          (e) => {
            try {
              onInput(field, JSON.parse((e.target as HTMLTextAreaElement).value));
            } catch {
              onInput(field, (e.target as HTMLTextAreaElement).value);
            }
          }
        "
      />
      <input
        v-else
        type="text"
        :value="String(valueFor(field) ?? '')"
        @input="onInput(field, ($event.target as HTMLInputElement).value)"
      />
    </div>
  </div>
</template>

<style scoped>
.schema-form-fields {
  display: flex;
  flex-direction: column;
  gap: 0.65rem;
}

.schema-form-fields--nested {
  margin-left: 0.5rem;
  padding-left: 0.75rem;
  border-left: 2px solid var(--border);
}

.schema-nested {
  margin-top: 0.25rem;
}

.const-pill {
  display: inline-block;
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 0.75rem;
  padding: 0.15rem 0.45rem;
  border-radius: var(--radius-sm);
  background: var(--surface-raised);
  border: 1px solid var(--border);
  color: var(--text-muted);
}

.schema-field-label {
  display: block;
  font-size: 0.75rem;
  font-weight: 600;
  margin-bottom: 0.25rem;
}

.required-mark {
  color: var(--color-danger, #c44);
}

.schema-field input[type="text"],
.schema-field input[type="number"],
.schema-field select,
.schema-field textarea {
  width: 100%;
  font-size: 0.8125rem;
}

.schema-field--focused {
  outline: 1px solid var(--color-accent);
  border-radius: var(--radius-sm);
  padding: 0.25rem;
}
</style>
