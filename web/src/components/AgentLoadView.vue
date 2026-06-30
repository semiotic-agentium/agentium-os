<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, ref } from "vue";
import DeploymentPanel from "./DeploymentPanel.vue";
import {
  buildPublishCommandFromFiles,
  summarizeAgentFiles,
  type PublishCommandPayload,
} from "../agent/sourceBundle";
import { usePublishApi } from "../composables/usePublishApi";
import { useToast } from "../composables/useToast";

const emit = defineEmits<{
  "agent-loaded": [];
  "chat-agent": [agentName: string];
}>();

const toast = useToast();
const { phase, error, lastHash, deployAfterPublish, loadAgent, reset } = usePublishApi();

const rationale = ref("Loaded from Agentium Console");
const selectedSummary = ref<{ rootDir: string; tsCount: number; bamlCount: number; hasManifest: boolean } | null>(
  null,
);
const pendingCommand = ref<PublishCommandPayload | null>(null);
const fileInputRef = ref<HTMLInputElement | null>(null);

const phaseLabel = computed(() => {
  switch (phase.value) {
    case "validating":
      return "Validating files…";
    case "publishing":
      return "Publishing (server build)…";
    case "deploying":
      return "Deploying…";
    case "done":
      return "Agent loaded";
    case "error":
      return "Failed";
    default:
      return "";
  }
});

const isBusy = computed(() =>
  ["validating", "publishing", "deploying"].includes(phase.value),
);

function openFolderPicker(): void {
  fileInputRef.value?.click();
}

async function onFolderSelected(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const files = input.files;
  if (!files?.length) return;
  selectedSummary.value = summarizeAgentFiles(files);
  reset();
  try {
    pendingCommand.value = await buildPublishCommandFromFiles(files, {
      rationale: rationale.value,
    });
  } catch (e) {
    pendingCommand.value = null;
    toast.error(e instanceof Error ? e.message : String(e));
  }
  input.value = "";
}

async function onLoadAgent(): Promise<void> {
  if (!pendingCommand.value) {
    toast.error("Select an agent folder first.");
    return;
  }
  pendingCommand.value.rationale = rationale.value.trim() || "Loaded from Agentium Console";
  const result = await loadAgent(pendingCommand.value);
  if (!result) {
    toast.error(error.value ?? "Load failed");
    return;
  }
  toast.success(`Loaded ${pendingCommand.value.name} (${result.hash.slice(0, 12)}…)`);
  emit("agent-loaded");
}

function onChatWithAgent(name: string): void {
  emit("chat-agent", name);
}
</script>

<template>
  <div class="agents-view">
    <header class="agents-view__header">
      <h2 class="agents-view__title">Agents on server</h2>
      <p class="agents-view__lede">
        Publish agent source to the connected instance and deploy it. Same flow as
        <code>agentium install agent</code>.
      </p>
    </header>

    <section class="agents-view__load panel">
      <h3 class="agents-view__section-title">Load agent</h3>
      <div class="agents-view__load-row">
        <input
          ref="fileInputRef"
          type="file"
          class="sr-only"
          webkitdirectory
          directory
          multiple
          @change="onFolderSelected"
        />
        <button type="button" class="btn btn--secondary" :disabled="isBusy" @click="openFolderPicker">
          Select agent folder…
        </button>
        <label class="agents-view__checkbox">
          <input v-model="deployAfterPublish" type="checkbox" :disabled="isBusy" />
          Deploy after publish
        </label>
      </div>

      <label class="agents-view__field">
        <span class="agents-view__label">Change rationale</span>
        <input v-model="rationale" class="input" type="text" :disabled="isBusy" />
      </label>

      <div v-if="selectedSummary" class="agents-view__summary">
        <strong>{{ selectedSummary.rootDir || "agent" }}</strong>
        — manifest {{ selectedSummary.hasManifest ? "ok" : "missing" }},
        {{ selectedSummary.tsCount }} TS,
        {{ selectedSummary.bamlCount }} BAML
      </div>

      <div v-if="phaseLabel" class="agents-view__phase" :data-phase="phase">
        {{ phaseLabel }}
        <span v-if="lastHash && phase === 'done'" class="agents-view__hash">{{ lastHash }}</span>
      </div>
      <p v-if="error" class="agents-view__error" role="alert">{{ error }}</p>

      <button
        type="button"
        class="btn btn--primary"
        :disabled="isBusy || !pendingCommand"
        @click="onLoadAgent"
      >
        {{ isBusy ? phaseLabel : "Load agent" }}
      </button>
    </section>

    <section class="agents-view__fleet panel">
      <DeploymentPanel @chat-agent="onChatWithAgent" />
    </section>
  </div>
</template>

<style scoped>
.agents-view {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 20px 24px 32px;
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.agents-view__header {
  max-width: 720px;
}

.agents-view__title {
  margin: 0 0 6px;
  font-size: var(--text-2xl);
  font-weight: 600;
}

.agents-view__lede {
  margin: 0;
  color: var(--text-secondary);
  font-size: var(--text-md);
  line-height: 1.5;
}

.agents-view__section-title {
  margin: 0 0 12px;
  font-size: var(--text-lg);
  font-weight: 600;
}

.agents-view__load {
  padding: 16px 18px;
  max-width: 640px;
}

.agents-view__load-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}

.agents-view__checkbox {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: var(--text-md);
  color: var(--text-secondary);
}

.agents-view__field {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 12px;
}

.agents-view__label {
  font-size: var(--text-sm);
  color: var(--text-secondary);
}

.agents-view__summary {
  margin-bottom: 10px;
  font-size: var(--text-md);
  color: var(--text-secondary);
}

.agents-view__phase {
  margin-bottom: 8px;
  font-size: var(--text-md);
  color: var(--text-secondary);
}

.agents-view__phase[data-phase="done"] {
  color: var(--color-success);
}

.agents-view__phase[data-phase="error"] {
  color: var(--color-error);
}

.agents-view__hash {
  display: block;
  margin-top: 4px;
  font-family: var(--font-mono);
  font-size: var(--text-sm);
  word-break: break-all;
}

.agents-view__error {
  margin: 0 0 10px;
  color: var(--color-error);
  font-size: var(--text-md);
}

.agents-view__fleet {
  padding: 0;
  overflow: hidden;
}

.agents-view__fleet :deep(.deployment-panel) {
  border: none;
  box-shadow: none;
}
</style>
