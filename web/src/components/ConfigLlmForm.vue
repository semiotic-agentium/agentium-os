<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { LlmClientConfig, LlmClientDef } from "../types/config";
import { LLM_PROVIDERS } from "../types/config";

const props = defineProps<{
  modelValue: LlmClientConfig;
  defaultClientName: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: LlmClientConfig];
  save: [value: LlmClientConfig];
}>();

const local = ref<LlmClientConfig>({ ...props.modelValue });
watch(
  () => props.modelValue,
  (v) => {
    local.value = {
      default: v.default,
      clients: JSON.parse(JSON.stringify(v.clients)),
      overrides: {
        agent: { ...v.overrides?.agent },
        agent_function: { ...v.overrides?.agent_function },
      },
      retry_policies: v.retry_policies ? { ...v.retry_policies } : {},
    };
  },
  { immediate: true },
);

const clientNames = computed(() => Object.keys(local.value.clients));

/** Group client names by provider for display. Default client is always first (its provider group first, then default first within that group). */
const clientsByProvider = computed(() => {
  const map: Record<string, string[]> = {};
  for (const name of clientNames.value) {
    const p = getClient(name).provider;
    const key = typeof p === "string" ? p : String(p);
    if (!map[key]) map[key] = [];
    map[key].push(name);
  }
  const defaultName = local.value.default;
  for (const key of Object.keys(map)) {
    const list = map[key];
    if (!list) continue;
    list.sort((a, b) => {
      if (a === defaultName) return -1;
      if (b === defaultName) return 1;
      return a.localeCompare(b);
    });
  }
  const keys = Object.keys(map);
  if (defaultName && keys.length > 1) {
    const defaultProvider = getClient(defaultName).provider;
    const providerKey = typeof defaultProvider === "string" ? defaultProvider : String(defaultProvider);
    const idx = keys.indexOf(providerKey);
    if (idx > 0) {
      keys.splice(idx, 1);
      keys.unshift(providerKey);
    }
  }
  const ordered: Record<string, string[]> = {};
  for (const k of keys) {
    if (map[k]) ordered[k] = map[k];
  }
  return ordered;
});

const defaultClientName = computed({
  get: () => local.value.default,
  set: (name: string) => {
    local.value.default = name;
    emit("update:modelValue", local.value);
  },
});

function getClient(name: string): LlmClientDef {
  return local.value.clients[name] ?? { name, provider: "openrouter", options: {} };
}

function setClient(name: string, def: LlmClientDef) {
  const clients = { ...local.value.clients };
  if (def.name !== name && clients[name]) {
    delete clients[name];
  }
  clients[def.name] = def;
  local.value.clients = clients;
  if (local.value.default === name && def.name !== name) {
    local.value.default = def.name;
  }
  emit("update:modelValue", local.value);
}

function addClient() {
  const base = "NewClient";
  let n = 1;
  while (local.value.clients[`${base}${n}`]) n++;
  const name = `${base}${n}`;
  setClient(name, { name, provider: "openrouter", options: {} });
}

function removeClient(name: string) {
  if (name === props.defaultClientName) return;
  const clients = { ...local.value.clients };
  delete clients[name];
  local.value.clients = clients;
  if (local.value.default === name) {
    local.value.default = clientNames.value[0] ?? "";
  }
  emit("update:modelValue", local.value);
}

const overridesAgent = computed({
  get: () => local.value.overrides?.agent ?? {},
  set: (v: Record<string, string>) => {
    local.value.overrides = { ...local.value.overrides, agent: v };
    emit("update:modelValue", local.value);
  },
});
const overridesAgentFunction = computed({
  get: () => local.value.overrides?.agent_function ?? {},
  set: (v: Record<string, string>) => {
    local.value.overrides = { ...local.value.overrides, agent_function: v };
    emit("update:modelValue", local.value);
  },
});

function onSave() {
  const payload: LlmClientConfig = {
    ...local.value,
    clients: { ...local.value.clients },
  };
  for (const name of Object.keys(payload.clients)) {
    const def = payload.clients[name];
    if (!def) continue;
    const options = { ...(def.options ?? {}) };
    delete options.api_key;
    payload.clients[name] = { ...def, name: def.name, options };
  }
  emit("save", payload);
}

/** User-editable options only (model, base_url). api_key is never shown or sent; backend injects it. */
function getModel(clientName: string): string {
  return getClient(clientName).options?.model ?? "";
}

function setModel(clientName: string, value: string) {
  const opts = { ...(getClient(clientName).options ?? {}) };
  delete opts.api_key;
  opts.model = value.trim();
  setClient(clientName, { ...getClient(clientName), options: opts });
}

function getBaseUrl(clientName: string): string {
  return getClient(clientName).options?.base_url ?? "";
}

function setBaseUrl(clientName: string, value: string) {
  const opts = { ...(getClient(clientName).options ?? {}) };
  delete opts.api_key;
  opts.base_url = value.trim();
  if (!opts.base_url) delete opts.base_url;
  setClient(clientName, { ...getClient(clientName), options: opts });
}

function overridesAgentText(): string {
  const o = local.value.overrides?.agent ?? {};
  return Object.entries(o)
    .map(([k, v]) => `${k}: ${v}`)
    .join("\n");
}

function overridesAgentFunctionText(): string {
  const o = local.value.overrides?.agent_function ?? {};
  return Object.entries(o)
    .map(([k, v]) => `${k}: ${v}`)
    .join("\n");
}

/** Parse "key: value" lines. For agent_function, key may contain colon (e.g. "agent:function"); split on last ": ". */
function parseOverrideLines(text: string, keyMayContainColon: boolean): Record<string, string> {
  const out: Record<string, string> = {};
  text.split("\n").forEach((l) => {
    const trimmed = l.trim();
    if (!trimmed) return;
    if (keyMayContainColon) {
      const lastColonSpace = trimmed.lastIndexOf(": ");
      if (lastColonSpace > 0) {
        out[trimmed.slice(0, lastColonSpace).trim()] = trimmed.slice(lastColonSpace + 2).trim();
      }
    } else {
      const i = trimmed.indexOf(":");
      if (i > 0) out[trimmed.slice(0, i).trim()] = trimmed.slice(i + 1).trim();
    }
  });
  return out;
}

function setOverridesAgentFromText(text: string) {
  overridesAgent.value = parseOverrideLines(text, false);
}

function setOverridesAgentFunctionFromText(text: string) {
  overridesAgentFunction.value = parseOverrideLines(text, true);
}
</script>

<template>
  <div class="config-llm-form">
    <div class="config-form-section">
      <h3 class="config-section-title">Default client</h3>
      <select v-model="defaultClientName" class="config-input config-select">
        <option v-for="name in clientNames" :key="name" :value="name">{{ name }}</option>
      </select>
    </div>

    <div class="config-form-section">
      <div class="config-section-header">
        <h3 class="config-section-title">Clients</h3>
        <button type="button" class="config-btn config-btn-secondary" @click="addClient">Add client</button>
      </div>
      <div v-for="(names, provider) in clientsByProvider" :key="provider" class="config-clients-by-provider">
        <h4 class="config-provider-group-title">{{ provider }}</h4>
        <div v-for="name in names" :key="name" class="config-client-card">
        <div class="config-client-header">
          <input
            :value="getClient(name).name"
            class="config-input config-input-inline"
            placeholder="Client name"
            @change="(e) => setClient(name, { ...getClient(name), name: (e.target as HTMLInputElement).value.trim() })"
          />
          <span v-if="name === defaultClientName" class="config-badge-default">Default</span>
          <button
            v-if="name !== defaultClientName"
            type="button"
            class="config-btn config-btn-ghost"
            title="Remove client"
            @click="removeClient(name)"
          >
            Remove
          </button>
        </div>
        <div class="config-client-fields">
          <label class="config-label">Provider</label>
          <select
            :value="getClient(name).provider"
            class="config-input config-select"
            @change="(e) => setClient(name, { ...getClient(name), provider: (e.target as HTMLSelectElement).value })"
          >
            <option v-for="p in LLM_PROVIDERS" :key="p" :value="p">{{ p }}</option>
          </select>
          <label class="config-label">Model</label>
          <input
            :value="getModel(name)"
            class="config-input"
            placeholder="e.g. openai/gpt-4o-mini or gpt-4o"
            @input="(e) => setModel(name, (e.target as HTMLInputElement).value)"
          />
          <label class="config-label">Base URL (optional, for self-hosted)</label>
          <input
            :value="getBaseUrl(name)"
            class="config-input"
            placeholder="e.g. https://openrouter.ai/api/v1"
            @input="(e) => setBaseUrl(name, (e.target as HTMLInputElement).value)"
          />
        </div>
        </div>
      </div>
    </div>

    <div class="config-form-section">
      <h3 class="config-section-title">Overrides (optional)</h3>
      <p class="config-hint">Agent package or agent:function → client name</p>
      <label class="config-label">Agent → client</label>
      <textarea
        :value="overridesAgentText()"
        class="config-input config-textarea"
        rows="2"
        placeholder="agent-package-name: ClientName"
        @input="(e) => setOverridesAgentFromText((e.target as HTMLTextAreaElement).value)"
      />
      <label class="config-label">Agent:function → client</label>
      <textarea
        :value="overridesAgentFunctionText()"
        class="config-input config-textarea"
        rows="2"
        placeholder="agent-package:function_name: ClientName"
        @input="(e) => setOverridesAgentFunctionFromText((e.target as HTMLTextAreaElement).value)"
      />
    </div>

    <div class="config-form-actions">
      <button type="button" class="config-btn config-btn-primary" @click="onSave">Save</button>
    </div>
  </div>
</template>
