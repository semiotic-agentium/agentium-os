<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import type { SecretRequestDto } from "../types/config";

const props = defineProps<{ bundleName: string; defaultConfig?: unknown }>();
const emit = defineEmits<{ close: [] }>();

const { fetchConfig, putConfig, fetchSecretRequests } = useConfigApi();
const configJson = ref("");
const savedJson = ref("");
const secretRequests = ref<SecretRequestDto[]>([]);
const error = ref<string | null>(null);
const saveMessage = ref<"saved" | "error" | null>(null);
const version = ref<number>(0);

const dirty = ref(false);

function beforeUnloadHandler(e: BeforeUnloadEvent) {
  if (dirty.value) {
    e.preventDefault();
  }
}

onMounted(async () => {
  window.addEventListener("beforeunload", beforeUnloadHandler);
  const [configResult, secretsResult] = await Promise.all([
    fetchConfig(props.bundleName),
    fetchSecretRequests(props.bundleName),
  ]);
  if ("error" in configResult) {
    error.value = configResult.error.detail ?? configResult.error.title;
    return;
  }
  const json = JSON.stringify(configResult.data.config, null, 2);
  configJson.value = json;
  savedJson.value = json;
  version.value = configResult.data.version;
  if ("data" in secretsResult) {
    secretRequests.value = secretsResult.data;
  }
});

onUnmounted(() => {
  window.removeEventListener("beforeunload", beforeUnloadHandler);
});

function onInput() {
  dirty.value = configJson.value !== savedJson.value;
  if (dirty.value) saveMessage.value = null;
}

function resetToDefault() {
  if (props.defaultConfig === undefined) return;
  const json = JSON.stringify(props.defaultConfig, null, 2);
  configJson.value = json;
  dirty.value = json !== savedJson.value;
}

function tryClose() {
  if (dirty.value && !confirm("You have unsaved changes. Discard them?")) {
    return;
  }
  emit("close");
}

async function save() {
  saveMessage.value = null;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(configJson.value) as Record<string, unknown>;
  } catch (e) {
    error.value = "Invalid JSON: " + (e instanceof Error ? e.message : String(e));
    return;
  }
  const result = await putConfig(props.bundleName, parsed, version.value);
  if ("error" in result) {
    saveMessage.value = "error";
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  version.value = result.data.version;
  savedJson.value = configJson.value;
  dirty.value = false;
  saveMessage.value = "saved";
  error.value = null;
}
</script>

<template>
  <div class="config-tool-editor">
    <div class="config-tool-editor-header">
      <h3 class="config-section-title">{{ bundleName }}</h3>
      <button type="button" class="config-btn config-btn-ghost" @click="tryClose">← Back to list</button>
    </div>

    <p v-if="error" class="config-error">{{ error }}</p>

    <div v-if="secretRequests.length > 0" class="config-form-section">
      <h4 class="config-section-title">Required secrets</h4>
      <p class="config-hint">Provision via your secret store; values are not entered here.</p>
      <ul class="config-secret-list">
        <li v-for="sr in secretRequests" :key="sr.name" class="config-secret-item">
          <strong>{{ sr.name }}</strong>
          <span v-if="sr.descriptor" class="config-secret-descriptor">{{ sr.descriptor }}</span>
          <span v-if="sr.justification" class="config-secret-justification">{{ sr.justification }}</span>
        </li>
      </ul>
    </div>

    <div class="config-form-section">
      <label class="config-label">Config (JSON)</label>
      <textarea
        v-model="configJson"
        class="config-input config-textarea config-json-editor"
        rows="16"
        spellcheck="false"
        @input="onInput"
      />
    </div>

    <div class="config-form-actions">
      <button type="button" class="config-btn config-btn-primary" @click="save">Save</button>
      <button
        v-if="defaultConfig !== undefined"
        type="button"
        class="config-btn config-btn-ghost"
        @click="resetToDefault"
      >Reset to default</button>
      <span v-if="dirty" class="config-dirty-indicator">Unsaved changes</span>
      <p v-if="saveMessage === 'saved'" class="config-save-ok">Saved (v{{ version }}).</p>
      <p v-else-if="saveMessage === 'error'" class="config-save-err">Save failed.</p>
    </div>
  </div>
</template>
