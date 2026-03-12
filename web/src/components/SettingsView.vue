<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import { LLM_BUNDLE_NAME } from "../types/config";
import type { ToolConfigSchemaDto } from "../types/config";
import ConfigLlmView from "./ConfigLlmView.vue";
import ConfigSecretsView from "./ConfigSecretsView.vue";
import ConfigToolBundleEditor from "./ConfigToolBundleEditor.vue";

const { fetchConfigList } = useConfigApi();
const configList = ref<ToolConfigSchemaDto[] | null>(null);
const configError = ref<string | null>(null);
const activeTab = ref<"llm" | "tools" | "secrets">("llm");
const selectedToolBundle = ref<string | null>(null);

const llmBundle = computed(() =>
  configList.value?.find((b) => b.tool_name === LLM_BUNDLE_NAME),
);
const toolBundles = computed(() =>
  configList.value?.filter((b) => b.tool_name !== LLM_BUNDLE_NAME) ?? [],
);

onMounted(async () => {
  const result = await fetchConfigList();
  if ("error" in result) {
    configError.value = result.error.detail ?? result.error.title;
    return;
  }
  configList.value = result.data;
});

const selectedToolDefault = ref<unknown>(undefined);

function openToolBundle(toolName: string) {
  selectedToolBundle.value = toolName;
  const bundle = toolBundles.value.find((b) => b.tool_name === toolName);
  selectedToolDefault.value = bundle?.default ?? undefined;
}

function closeToolEditor() {
  selectedToolBundle.value = null;
}
</script>

<template>
  <div class="settings-view">
    <div class="settings-scroll">
      <div class="settings-header">
        <h2 class="settings-title">Configuration</h2>
        <p class="settings-subtitle">LLM clients and tool bundle settings</p>
      </div>

      <template v-if="configError">
        <div class="settings-unavailable" role="alert" aria-live="assertive">
          <p>{{ configError }}</p>
          <p class="settings-unavailable-hint">Ensure the runner is started with config service and tool catalog.</p>
        </div>
      </template>

      <template v-else-if="configList">
        <div class="settings-tabs" role="tablist" aria-label="Config sections">
          <button
            type="button"
            role="tab"
            :aria-selected="activeTab === 'llm'"
            :class="['settings-tab', { active: activeTab === 'llm' }]"
            @click="activeTab = 'llm'"
          >
            LLM
          </button>
          <button
            type="button"
            role="tab"
            :aria-selected="activeTab === 'tools'"
            :class="['settings-tab', { active: activeTab === 'tools' }]"
            @click="activeTab = 'tools'; selectedToolBundle = null"
          >
            Tools
          </button>
          <button
            type="button"
            role="tab"
            :aria-selected="activeTab === 'secrets'"
            :class="['settings-tab', { active: activeTab === 'secrets' }]"
            @click="activeTab = 'secrets'"
          >
            Secrets
          </button>
        </div>

        <div class="settings-panels">
          <div v-if="activeTab === 'llm'" class="settings-panel">
            <ConfigLlmView v-if="llmBundle" />
            <p v-else class="settings-empty">LLM config bundle not available.</p>
          </div>

          <div v-else-if="activeTab === 'tools'" class="settings-panel">
            <template v-if="selectedToolBundle">
              <ConfigToolBundleEditor
                :bundle-name="selectedToolBundle"
                :default-config="selectedToolDefault"
                @close="closeToolEditor"
              />
            </template>
            <template v-else>
              <div class="tool-bundle-list">
                <p v-if="toolBundles.length === 0" class="settings-empty">No tool bundles with config.</p>
                <button
                  v-for="b in toolBundles"
                  :key="b.tool_name"
                  type="button"
                  class="tool-bundle-row"
                  @click="openToolBundle(b.tool_name)"
                >
                  <span class="tool-bundle-name">{{ b.tool_name }}</span>
                  <span v-if="b.has_config" class="tool-bundle-badge">Configured</span>
                </button>
              </div>
            </template>
          </div>

          <div v-else-if="activeTab === 'secrets'" class="settings-panel">
            <ConfigSecretsView />
          </div>
        </div>
      </template>

      <template v-else>
        <div class="settings-loading" role="status" aria-live="polite">Loading…</div>
      </template>
    </div>
  </div>
</template>
