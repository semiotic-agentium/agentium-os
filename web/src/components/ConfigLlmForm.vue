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

// ── Agent discovery ──

const discoveredAgents = ref<AgentDiscoveryEntry[]>([]);

onMounted(async () => {
  try {
    const res = await fetch("/agents");
    if (res.ok) {
      discoveredAgents.value = (await res.json()) as AgentDiscoveryEntry[];
    }
  } catch {
    // non-fatal
  }
});

function functionsForAgent(agentPackage: string): string[] {
  const agent = discoveredAgents.value.find((a) => a.agent_package === agentPackage);
  return agent?.agent_card?.baml_functions ?? [];
}

// ── Client list ──

const clientNames = computed(() => Object.keys(local.value.clients));

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
  if (def.name !== name && clients[name]) delete clients[name];
  clients[def.name] = def;
  local.value.clients = clients;
  if (local.value.default === name && def.name !== name) local.value.default = def.name;
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
  if (local.value.default === name) local.value.default = clientNames.value[0] ?? "";
  emit("update:modelValue", local.value);
}

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

// ── Overrides: card-per-agent model ──
//
// Three resolution levels:
//   1. System default  → local.value.default
//   2. Agent default   → local.value.overrides.agent[agentPkg]      (empty string = use system default)
//   3. Per-prompt      → local.value.overrides.agent_function["agentPkg:fn"]  (empty string = use agent default)

function getAgentOverride(agentPkg: string): string {
  return local.value.overrides?.agent?.[agentPkg] ?? "";
}

function setAgentOverride(agentPkg: string, client: string) {
  const agent = { ...(local.value.overrides?.agent ?? {}) };
  if (client) {
    agent[agentPkg] = client;
  } else {
    delete agent[agentPkg];
  }
  local.value.overrides = { ...local.value.overrides, agent };
  emit("update:modelValue", local.value);
}

function getFnOverride(agentPkg: string, fn: string): string {
  return local.value.overrides?.agent_function?.[`${agentPkg}:${fn}`] ?? "";
}

function setFnOverride(agentPkg: string, fn: string, client: string) {
  const af = { ...(local.value.overrides?.agent_function ?? {}) };
  const key = `${agentPkg}:${fn}`;
  if (client) {
    af[key] = client;
  } else {
    delete af[key];
  }
  local.value.overrides = { ...local.value.overrides, agent_function: af };
  emit("update:modelValue", local.value);
}

/** Effective inherited client label for a given agent (for display in function rows). */
function agentEffectiveLabel(agentPkg: string): string {
  return getAgentOverride(agentPkg) || local.value.default || "system default";
}

// ── Validation + Save ──

function validate(): string[] {
  const errors: string[] = [];
  const names = Object.keys(local.value.clients);
  if (names.length === 0) errors.push("At least one client is required.");
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
  const payload: LlmClientConfig = { ...local.value, clients: { ...local.value.clients } };
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

    <!-- ── System default ── -->
    <div class="config-form-section">
      <div class="config-override-system-row">
        <div class="config-override-system-label">
          <span class="config-section-title">System default</span>
          <span class="config-hint">All agents use this unless overridden below.</span>
        </div>
        <select v-model="defaultClientName" class="config-input config-select">
          <option v-for="name in clientNames" :key="name" :value="name">{{ name }}</option>
        </select>
      </div>
    </div>

    <!-- ── Per-agent override cards ── -->
    <div class="config-form-section">
      <h3 class="config-section-title">Agent routing</h3>
      <p class="config-hint">Set a default client per agent, and optionally override individual prompts.</p>

      <p v-if="discoveredAgents.length === 0" class="config-override-empty">
        No agents discovered — start the runner to populate this section.
      </p>

      <div
        v-for="agent in discoveredAgents"
        :key="agent.agent_package"
        class="config-agent-override-card"
      >
        <!-- Agent header + agent-level override -->
        <div class="config-agent-override-header">
          <span class="config-agent-override-name">{{ agent.agent_package }}</span>
          <div class="config-agent-override-client">
            <span class="config-override-inherit-label">uses</span>
            <select
              :value="getAgentOverride(agent.agent_package)"
              class="config-input config-select"
              :aria-label="'Default client for ' + agent.agent_package"
              @change="(e) => setAgentOverride(agent.agent_package, (e.target as HTMLSelectElement).value)"
            >
              <option value="">{{ local.default }} (system default)</option>
              <option
                v-for="c in clientNames"
                :key="c"
                :value="c"
              >{{ c }}</option>
            </select>
          </div>
        </div>

        <!-- Per-function overrides -->
        <div
          v-if="functionsForAgent(agent.agent_package).length > 0"
          class="config-fn-override-list"
        >
          <div
            v-for="fn in functionsForAgent(agent.agent_package)"
            :key="fn"
            class="config-fn-override-row"
          >
            <span class="config-fn-override-name">{{ fn }}</span>
            <div class="config-fn-override-client">
              <select
                :value="getFnOverride(agent.agent_package, fn)"
                class="config-input config-select"
                :aria-label="fn + ' client override'"
                @change="(e) => setFnOverride(agent.agent_package, fn, (e.target as HTMLSelectElement).value)"
              >
                <option value="">{{ agentEffectiveLabel(agent.agent_package) }} (inherited)</option>
                <option
                  v-for="c in clientNames"
                  :key="c"
                  :value="c"
                >{{ c }}</option>
              </select>
            </div>
          </div>
        </div>
        <p v-else class="config-fn-override-empty">No BAML functions discovered for this agent.</p>
      </div>
    </div>

    <!-- ── LLM clients ── -->
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
            >Remove</button>
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

    <ul v-if="validationErrors.length > 0" class="config-validation-errors">
      <li v-for="(err, errIdx) in validationErrors" :key="errIdx" class="config-error">{{ err }}</li>
    </ul>

    <div class="config-form-actions">
      <button type="button" class="config-btn config-btn-primary" @click="onSave">Save</button>
    </div>
  </div>
</template>
