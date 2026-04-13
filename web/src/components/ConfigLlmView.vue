<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import { useToast } from "../composables/useToast";
import type { LlmClientConfig } from "../types/config";
import ConfigLlmForm from "./ConfigLlmForm.vue";

const { fetchConfig, putConfig } = useConfigApi();
const toast = useToast();
const config = ref<LlmClientConfig | null>(null);
const error = ref<string | null>(null);
const saveStatus = ref<"idle" | "saving" | "saved" | "error">("idle");
const version = ref<number>(0);
let loaded = false;
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

function beforeUnloadHandler(e: BeforeUnloadEvent) {
  if (saveStatus.value === "saving") e.preventDefault();
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
  // Mark loaded after the next tick so the initial prop assignment
  // does not trigger the watcher.
  setTimeout(() => {
    loaded = true;
  }, 0);
});

onUnmounted(() => {
  window.removeEventListener("beforeunload", beforeUnloadHandler);
  if (debounceTimer) clearTimeout(debounceTimer);
});

// Auto-save: debounce every model change by 800ms.
watch(
  config,
  () => {
    if (!loaded || !config.value) return;
    saveStatus.value = "saving";
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => doSave(config.value!), 800);
  },
  { deep: true },
);

async function doSave(payload: LlmClientConfig) {
  // Strip api_key from options before sending — same logic as the form's onSave.
  const clean: LlmClientConfig = { ...payload, clients: { ...payload.clients } };
  for (const name of Object.keys(clean.clients)) {
    const def = clean.clients[name];
    if (!def) continue;
    const options = { ...(def.options ?? {}) };
    delete options.api_key;
    clean.clients[name] = { ...def, options };
  }

  const result = await putConfig("llm", clean as unknown as Record<string, unknown>, version.value);
  if ("error" in result) {
    saveStatus.value = "error";
    error.value = result.error.detail ?? result.error.title;
    toast.error("LLM config save failed");
    return;
  }
  version.value = result.data.version;
  saveStatus.value = "saved";
  error.value = null;
  toast.success(`LLM config saved (v${version.value})`);
}
</script>

<template>
  <div class="config-llm-view">
    <div class="config-autosave-bar">
      <span v-if="saveStatus === 'saving'" class="config-autosave-saving">Saving…</span>
      <span v-else-if="saveStatus === 'saved'" class="config-autosave-saved">Saved (v{{ version }})</span>
      <span v-else-if="saveStatus === 'error'" class="config-autosave-error">Save failed</span>
    </div>
    <p v-if="error" class="config-error">{{ error }}</p>
    <ConfigLlmForm v-if="config" v-model="config" :default-client-name="config.default" />
    <p v-else-if="!error" class="settings-empty">Loading…</p>
  </div>
</template>
