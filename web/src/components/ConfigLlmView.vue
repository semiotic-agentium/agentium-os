<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import type { LlmClientConfig } from "../types/config";
import ConfigLlmForm from "./ConfigLlmForm.vue";

const { fetchConfig, putConfig } = useConfigApi();
const config = ref<LlmClientConfig | null>(null);
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
  const result = await fetchConfig("llm");
  if ("error" in result) {
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  version.value = result.data.version;
  const raw = result.data.config as unknown;
  if (raw && typeof raw === "object" && "default" in raw && "clients" in raw) {
    config.value = raw as LlmClientConfig;
  } else {
    config.value = { default: "", clients: {}, overrides: {} };
  }
});

onUnmounted(() => {
  window.removeEventListener("beforeunload", beforeUnloadHandler);
});

function onFormUpdate() {
  dirty.value = true;
  saveMessage.value = null;
}

async function onSave(payload: LlmClientConfig) {
  saveMessage.value = null;
  const result = await putConfig(
    "llm",
    payload as unknown as Record<string, unknown>,
    version.value,
  );
  if ("error" in result) {
    saveMessage.value = "error";
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  version.value = result.data.version;
  saveMessage.value = "saved";
  config.value = payload;
  dirty.value = false;
  error.value = null;
}
</script>

<template>
  <div class="config-llm-view">
    <p v-if="error" class="config-error">{{ error }}</p>
    <template v-else-if="config">
      <ConfigLlmForm
        :model-value="config"
        :default-client-name="config.default"
        @update:model-value="onFormUpdate"
        @save="onSave"
      />
      <p v-if="saveMessage === 'saved'" class="config-save-ok">Saved (v{{ version }}).</p>
      <p v-else-if="saveMessage === 'error'" class="config-save-err">Save failed. See error above.</p>
    </template>
    <p v-else class="settings-empty">Loading…</p>
  </div>
</template>
