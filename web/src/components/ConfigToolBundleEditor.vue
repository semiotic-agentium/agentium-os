<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import type { SecretRequestDto } from "../types/config";

const props = defineProps<{ bundleName: string }>();
const emit = defineEmits<{ close: [] }>();

const { fetchConfig, putConfig, fetchSecretRequests } = useConfigApi();
const configJson = ref("");
const secretRequests = ref<SecretRequestDto[]>([]);
const error = ref<string | null>(null);
const saveMessage = ref<"saved" | "error" | null>(null);

onMounted(async () => {
  const [configResult, secretsResult] = await Promise.all([
    fetchConfig(props.bundleName),
    fetchSecretRequests(props.bundleName),
  ]);
  if ("error" in configResult) {
    error.value = configResult.error.detail ?? configResult.error.title;
    return;
  }
  configJson.value = JSON.stringify(configResult.data.config, null, 2);
  if ("data" in secretsResult) {
    secretRequests.value = secretsResult.data;
  }
});

async function save() {
  saveMessage.value = null;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(configJson.value) as Record<string, unknown>;
  } catch (e) {
    error.value = "Invalid JSON: " + (e instanceof Error ? e.message : String(e));
    return;
  }
  const result = await putConfig(props.bundleName, parsed);
  if ("error" in result) {
    saveMessage.value = "error";
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  saveMessage.value = "saved";
  error.value = null;
  setTimeout(() => { saveMessage.value = null; }, 2000);
}
</script>

<template>
  <div class="config-tool-editor">
    <div class="config-tool-editor-header">
      <h3 class="config-section-title">{{ bundleName }}</h3>
      <button type="button" class="config-btn config-btn-ghost" @click="emit('close')">← Back to list</button>
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
      />
    </div>

    <div class="config-form-actions">
      <button type="button" class="config-btn config-btn-primary" @click="save">Save</button>
      <p v-if="saveMessage === 'saved'" class="config-save-ok">Saved.</p>
      <p v-else-if="saveMessage === 'error'" class="config-save-err">Save failed.</p>
    </div>
  </div>
</template>
