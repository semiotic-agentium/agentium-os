<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import type { AgentDiscoveryEntry } from "../types/a2a";
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

const validationErrors = ref<string[]>([]);

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

// ── Agent discovery for override dropdowns ──

const discoveredAgents = ref<AgentDiscoveryEntry[]>([]);

onMounted(async () => {
  try {
    const res = await fetch("/agents");
    if (res.ok) {
      discoveredAgents.value = (await res.json()) as AgentDiscoveryEntry[];
    }
  } catch {
    // Non-fatal; dropdowns just show empty agents list
  }
});

const agentPackages = computed(() =>
  discoveredAgents.value.map((a) => a.agent_package),
);

function functionsForAgent(agentPackage: string): string[] {
  const agent = discoveredAgents.value.find((a) => a.agent_package === agentPackage);
  return agent?.agent_card?.baml_functions ?? [];
}

// ── Client list ──

const clientNames = computed(() => Object.keys(local.value.clients));

/** Group client names by provider for display. Default client is always first. */
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

// ── Agent overrides (agent → client) ──

interface AgentOverrideRow {
  agent: string;
  client: string;
}

const agentOverrideRows = computed<AgentOverrideRow[]>(() =>
  Object.entries(local.value.overrides?.agent ?? {}).map(([agent, client]) => ({
    agent,
    client,
  })),
);

function setAgentOverrides(rows: AgentOverrideRow[]) {
  const map: Record<string, string> = {};
  for (const r of rows) {
    if (r.agent && r.client) map[r.agent] = r.client;
  }
  local.value.overrides = { ...local.value.overrides, agent: map };
  emit("update:modelValue", local.value);
}

function addAgentOverride() {
  const rows = [...agentOverrideRows.value, { agent: "", client: "" }];
  setAgentOverrides(rows);
}

function updateAgentOverrideAgent(idx: number, agent: string) {
  const rows = agentOverrideRows.value.map((r, i) =>
    i === idx ? { ...r, agent } : r,
  );
  setAgentOverrides(rows);
}

function updateAgentOverrideClient(idx: number, client: string) {
  const rows = agentOverrideRows.value.map((r, i) =>
    i === idx ? { ...r, client } : r,
  );
  setAgentOverrides(rows);
}

function removeAgentOverride(idx: number) {
  const rows = agentOverrideRows.value.filter((_, i) => i !== idx);
  setAgentOverrides(rows);
}

// ── Agent:function overrides (agent + function → client) ──

interface FnOverrideRow {
  agent: string;
  fn: string;
  client: string;
}

const fnOverrideRows = computed<FnOverrideRow[]>(() =>
  Object.entries(local.value.overrides?.agent_function ?? {}).map(([key, client]) => {
    const colon = key.indexOf(":");
    const agent = colon >= 0 ? key.slice(0, colon) : key;
    const fn = colon >= 0 ? key.slice(colon + 1) : "";
    return { agent, fn, client };
  }),
);

function setFnOverrides(rows: FnOverrideRow[]) {
  const map: Record<string, string> = {};
  for (const r of rows) {
    if (r.agent && r.fn && r.client) map[`${r.agent}:${r.fn}`] = r.client;
  }
  local.value.overrides = { ...local.value.overrides, agent_function: map };
  emit("update:modelValue", local.value);
}

function addFnOverride() {
  const rows = [...fnOverrideRows.value, { agent: "", fn: "", client: "" }];
  setFnOverrides(rows);
}

function updateFnOverrideAgent(idx: number, agent: string) {
  const rows = fnOverrideRows.value.map((r, i) =>
    i === idx ? { ...r, agent, fn: "" } : r,
  );
  setFnOverrides(rows);
}

function updateFnOverrideFn(idx: number, fn: string) {
  const rows = fnOverrideRows.value.map((r, i) =>
    i === idx ? { ...r, fn } : r,
  );
  setFnOverrides(rows);
}

function updateFnOverrideClient(idx: number, client: string) {
  const rows = fnOverrideRows.value.map((r, i) =>
    i === idx ? { ...r, client } : r,
  );
  setFnOverrides(rows);
}

function removeFnOverride(idx: number) {
  const rows = fnOverrideRows.value.filter((_, i) => i !== idx);
  setFnOverrides(rows);
}

// ── Validation + Save ──

function validate(): string[] {
  const errors: string[] = [];
  const names = Object.keys(local.value.clients);
  if (names.length === 0) {
    errors.push("At least one client is required.");
  }
  const seen = new Set<string>();
  for (const name of names) {
    const trimmed = name.trim();
    if (!trimmed) {
      errors.push("Client name cannot be empty.");
    } else if (seen.has(trimmed)) {
      errors.push(`Duplicate client name: "${trimmed}".`);
    }
    seen.add(trimmed);
    const def = local.value.clients[name];
    if (def && !def.options?.model?.trim()) {
      errors.push(`Client "${trimmed}" has no model specified.`);
    }
  }
  if (local.value.default && !local.value.clients[local.value.default]) {
    errors.push(`Default client "${local.value.default}" does not exist.`);
  }
  return errors;
}

function onSave() {
  const errors = validate();
  validationErrors.value = errors;
  if (errors.length > 0) return;

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
            <label class="config-label">Base URL</label>
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

    <!-- ── Agent overrides ── -->
    <div class="config-form-section">
      <div class="config-section-header">
        <h3 class="config-section-title">Agent overrides</h3>
        <button type="button" class="config-btn config-btn-secondary config-override-add" @click="addAgentOverride">
          Add
        </button>
      </div>
      <p class="config-hint">Route all calls from a specific agent to a given LLM client.</p>

      <div class="config-override-rows">
        <p v-if="agentOverrideRows.length === 0" class="config-override-empty">
          No agent overrides — all agents use the default client.
        </p>
        <div
          v-for="(row, i) in agentOverrideRows"
          :key="i"
          class="config-override-row"
        >
          <select
            :value="row.agent"
            class="config-input"
            :aria-label="'Agent for override ' + (i + 1)"
            @change="(e) => updateAgentOverrideAgent(i, (e.target as HTMLSelectElement).value)"
          >
            <option value="">Select agent…</option>
            <option v-for="pkg in agentPackages" :key="pkg" :value="pkg">{{ pkg }}</option>
          </select>
          <span class="config-override-arrow">→</span>
          <select
            :value="row.client"
            class="config-input"
            :aria-label="'Client for override ' + (i + 1)"
            @change="(e) => updateAgentOverrideClient(i, (e.target as HTMLSelectElement).value)"
          >
            <option value="">Select client…</option>
            <option v-for="c in clientNames" :key="c" :value="c">{{ c }}</option>
          </select>
          <button
            type="button"
            class="config-btn config-btn-ghost"
            :aria-label="'Remove agent override ' + (i + 1)"
            @click="removeAgentOverride(i)"
          >✕</button>
        </div>
      </div>
    </div>

    <!-- ── Agent:function overrides ── -->
    <div class="config-form-section">
      <div class="config-section-header">
        <h3 class="config-section-title">Function overrides</h3>
        <button type="button" class="config-btn config-btn-secondary config-override-add" @click="addFnOverride">
          Add
        </button>
      </div>
      <p class="config-hint">Route a specific BAML function in an agent to a given LLM client.</p>

      <div class="config-override-rows">
        <p v-if="fnOverrideRows.length === 0" class="config-override-empty">
          No function overrides — all functions use the agent or default client.
        </p>
        <div
          v-for="(row, i) in fnOverrideRows"
          :key="i"
          class="config-override-row-fn"
        >
          <select
            :value="row.agent"
            class="config-input"
            :aria-label="'Agent for fn override ' + (i + 1)"
            @change="(e) => updateFnOverrideAgent(i, (e.target as HTMLSelectElement).value)"
          >
            <option value="">Select agent…</option>
            <option v-for="pkg in agentPackages" :key="pkg" :value="pkg">{{ pkg }}</option>
          </select>
          <select
            :value="row.fn"
            class="config-input"
            :disabled="!row.agent"
            :aria-label="'Function for fn override ' + (i + 1)"
            @change="(e) => updateFnOverrideFn(i, (e.target as HTMLSelectElement).value)"
          >
            <option value="">Select function…</option>
            <option
              v-for="fn in functionsForAgent(row.agent)"
              :key="fn"
              :value="fn"
            >{{ fn }}</option>
          </select>
          <span class="config-override-arrow">→</span>
          <select
            :value="row.client"
            class="config-input"
            :aria-label="'Client for fn override ' + (i + 1)"
            @change="(e) => updateFnOverrideClient(i, (e.target as HTMLSelectElement).value)"
          >
            <option value="">Select client…</option>
            <option v-for="c in clientNames" :key="c" :value="c">{{ c }}</option>
          </select>
          <button
            type="button"
            class="config-btn config-btn-ghost"
            :aria-label="'Remove fn override ' + (i + 1)"
            @click="removeFnOverride(i)"
          >✕</button>
        </div>
      </div>
    </div>

    <ul v-if="validationErrors.length > 0" class="config-validation-errors">
      <li v-for="(err, errIdx) in validationErrors" :key="errIdx" class="config-error">{{ err }}</li>
    </ul>

    <div class="config-form-actions">
      <button type="button" class="config-btn config-btn-primary" @click="onSave">Save</button>
    </div>
  </div>
</template>
