<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import type { LlmClientConfig } from "../types/config";
import ConfigLlmForm from "./ConfigLlmForm.vue";

const { fetchConfig, putConfig } = useConfigApi();
const config = ref<LlmClientConfig | null>(null);
const error = ref<string | null>(null);
const saveMessage = ref<"saved" | "error" | null>(null);

onMounted(async () => {
  const result = await fetchConfig("llm");
  if ("error" in result) {
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  const raw = result.data.config as unknown;
  if (raw && typeof raw === "object" && "default" in raw && "clients" in raw) {
    config.value = raw as LlmClientConfig;
  } else {
    config.value = { default: "", clients: {}, overrides: {} };
  }
});

async function onSave(payload: LlmClientConfig) {
  saveMessage.value = null;
  const result = await putConfig("llm", payload as unknown as Record<string, unknown>);
  if ("error" in result) {
    saveMessage.value = "error";
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  saveMessage.value = "saved";
  config.value = payload;
  error.value = null;
  setTimeout(() => { saveMessage.value = null; }, 2000);
}
</script>

<template>
  <div class="config-llm-view">
    <p v-if="error" class="config-error">{{ error }}</p>
    <template v-else-if="config">
      <ConfigLlmForm
        :model-value="config"
        :default-client-name="config.default"
        @save="onSave"
      />
      <p v-if="saveMessage === 'saved'" class="config-save-ok">Saved.</p>
      <p v-else-if="saveMessage === 'error'" class="config-save-err">Save failed. See error above.</p>
    </template>
    <p v-else class="settings-empty">Loading…</p>
  </div>
</template>
