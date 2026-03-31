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
const saveStatus = ref<"idle" | "saving" | "saved" | "error">("idle");
const version = ref<number>(0);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

function beforeUnloadHandler(e: BeforeUnloadEvent) {
  if (saveStatus.value === "saving") e.preventDefault();
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
  if (debounceTimer) clearTimeout(debounceTimer);
});

function onInput() {
  error.value = null;
  if (configJson.value === savedJson.value) {
    saveStatus.value = "idle";
    return;
  }
  saveStatus.value = "saving";
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = setTimeout(doSave, 1200);
}

async function doSave() {
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(configJson.value) as Record<string, unknown>;
  } catch (e) {
    saveStatus.value = "error";
    error.value = "Invalid JSON: " + (e instanceof Error ? e.message : String(e));
    return;
  }
  const result = await putConfig(props.bundleName, parsed, version.value);
  if ("error" in result) {
    saveStatus.value = "error";
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  version.value = result.data.version;
  savedJson.value = configJson.value;
  saveStatus.value = "saved";
  error.value = null;
}

function resetToDefault() {
  if (props.defaultConfig === undefined) return;
  configJson.value = JSON.stringify(props.defaultConfig, null, 2);
  onInput();
}

function tryClose() {
  if (saveStatus.value === "saving") {
    if (!confirm("A save is in progress. Close anyway?")) return;
  }
  emit("close");
}
</script>

<template>
  <div class="config-tool-editor">
    <div class="config-tool-editor-header">
      <h3 class="config-section-title">{{ bundleName }}</h3>
      <div style="display:flex;align-items:center;gap:12px;">
        <span v-if="saveStatus === 'saving'" class="config-autosave-saving">Saving…</span>
        <span v-else-if="saveStatus === 'saved'" class="config-autosave-saved">Saved (v{{ version }})</span>
        <span v-else-if="saveStatus === 'error'" class="config-autosave-error">Save failed</span>
        <button type="button" class="btn btn--ghost btn--sm" @click="tryClose">← Back</button>
      </div>
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

    <div v-if="defaultConfig !== undefined" class="config-form-actions">
      <button
        type="button"
        class="btn btn--ghost btn--sm"
        @click="resetToDefault"
      >Reset to default</button>
    </div>
  </div>
</template>
