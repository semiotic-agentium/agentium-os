<script setup lang="ts">
import { computed, ref } from "vue";
import EventSchemaFormFields from "./EventSchemaFormFields.vue";
import { ensureObjectPayload } from "../../events/schemaForm";
import type { AgentDeliverableMessageShape } from "../../types/events";

const props = defineProps<{
  messageShape: AgentDeliverableMessageShape | null;
  messages: unknown[];
  activeIndex: number;
  validationFocusPath?: string | null;
}>();

const emit = defineEmits<{
  "update:messages": [unknown[]];
  "update:activeIndex": [number];
  add: [];
  duplicate: [number];
  remove: [number];
}>();

const editorTab = ref<"form" | "json" | "preview">("form");

const activeMessage = computed(() =>
  ensureObjectPayload(props.messages[props.activeIndex]),
);

const formLabels = computed(() => {
  const shape = props.messageShape;
  if (!shape) return undefined;
  const labels = { ...(shape.ui_hints?.field_labels ?? {}) };
  const pointer = shape.ui_hints?.primary_record_array_pointer;
  if (pointer && shape.payload_name) {
    const path = pointer.replace(/^\//, "").replace(/\//g, ".");
    labels[path] = shape.payload_name;
  }
  return labels;
});

const jsonText = computed({
  get() {
    return JSON.stringify(activeMessage.value, null, 2);
  },
  set(raw: string) {
    try {
      const parsed = JSON.parse(raw) as unknown;
      const next = [...props.messages];
      next[props.activeIndex] = parsed;
      emit("update:messages", next);
    } catch {
      // preserve last valid structured state
    }
  },
});

const previewRequest = computed(() => ({
  messages: props.messages,
}));

function updateActive(model: Record<string, unknown>): void {
  const next = [...props.messages];
  next[props.activeIndex] = model;
  emit("update:messages", next);
}
</script>

<template>
  <div class="event-batch-editor">
    <div class="batch-rail">
      <button type="button" class="batch-add" @click="emit('add')">+ Message</button>
      <button
        v-for="(_, i) in messages"
        :key="i"
        type="button"
        :class="['batch-item', { active: i === activeIndex }]"
        @click="emit('update:activeIndex', i)"
      >
        Message {{ i + 1 }}
      </button>
      <button
        v-if="messages.length > 0"
        type="button"
        class="batch-action"
        @click="emit('duplicate', activeIndex)"
      >
        Duplicate
      </button>
      <button
        v-if="messages.length > 1"
        type="button"
        class="batch-action"
        @click="emit('remove', activeIndex)"
      >
        Delete
      </button>
    </div>

    <div v-if="messageShape" class="payload-header">
      <h3 class="payload-title">{{ messageShape.payload_name }}</h3>
      <span class="payload-origin">{{ messageShape.origin }}</span>
    </div>

    <div v-if="messageShape" class="editor-tabs">
      <button
        type="button"
        :class="{ active: editorTab === 'form' }"
        @click="editorTab = 'form'"
      >
        Form
      </button>
      <button
        type="button"
        :class="{ active: editorTab === 'json' }"
        @click="editorTab = 'json'"
      >
        JSON
      </button>
      <button
        type="button"
        :class="{ active: editorTab === 'preview' }"
        @click="editorTab = 'preview'"
      >
        Preview batch
      </button>
    </div>

    <EventSchemaFormFields
      v-if="messageShape && editorTab === 'form'"
      :schema="messageShape.payload_schema"
      :model="activeMessage"
      :labels="formLabels"
      :focus-path="validationFocusPath"
      @update="updateActive"
    />
    <textarea
      v-else-if="editorTab === 'json'"
      v-model="jsonText"
      class="json-editor"
      rows="16"
      spellcheck="false"
    />
    <pre v-else class="preview-pre">{{ JSON.stringify(previewRequest, null, 2) }}</pre>
  </div>
</template>

<style scoped>
.event-batch-editor {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.payload-header {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.5rem 1rem;
}

.payload-title {
  margin: 0;
  font-size: 0.9375rem;
  font-weight: 600;
}

.payload-origin {
  font-size: 0.75rem;
  font-family: var(--font-mono, ui-monospace, monospace);
  color: var(--text-muted);
}

.batch-rail {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem;
  align-items: center;
}

.batch-item,
.batch-add,
.batch-action {
  font-size: 0.75rem;
  padding: 0.25rem 0.5rem;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
}

.batch-item.active {
  border-color: var(--color-accent);
  background: var(--surface-raised);
}

.editor-tabs {
  display: flex;
  gap: 0.35rem;
}

.editor-tabs button {
  font-size: 0.75rem;
  padding: 0.25rem 0.6rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface);
}

.editor-tabs button.active {
  border-color: var(--color-accent);
}

.json-editor,
.preview-pre {
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 0.8125rem;
  width: 100%;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  padding: 0.5rem;
  background: var(--surface-raised);
}
</style>
