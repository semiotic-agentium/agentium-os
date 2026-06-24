<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import type { AgentDiscoveryEntry } from "../types/a2a";
import type {
  LlmClientConfig,
  LlmClientDef,
  ModelBudgetOverride,
  ModelContextBudget,
  ResolvedClientBudgets,
} from "../types/config";
import { LLM_PROVIDERS } from "../types/config";

const props = defineProps<{
  modelValue: LlmClientConfig;
  defaultClientName: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: LlmClientConfig];
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
      compaction: v.compaction
        ? JSON.parse(JSON.stringify(v.compaction))
        : { defaults: {}, model_overrides: {}, client_overrides: {}, online_sources: {} },
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
  await loadBudgets();
});

const resolvedBudgets = ref<ModelContextBudget[]>([]);
const budgetLoadError = ref<string | null>(null);
const budgetRefreshing = ref(false);

async function loadBudgets() {
  budgetLoadError.value = null;
  try {
    const res = await fetch("/config/llm/model-budgets");
    if (!res.ok) {
      budgetLoadError.value = `Failed to load budgets (${res.status})`;
      return;
    }
    const data = (await res.json()) as ResolvedClientBudgets;
    resolvedBudgets.value = data.clients ?? [];
  } catch {
    budgetLoadError.value = "Failed to load compaction budgets";
  }
}

async function refreshBudgetMetadata() {
  budgetRefreshing.value = true;
  budgetLoadError.value = null;
  try {
    const res = await fetch("/config/llm/model-budgets/refresh", { method: "POST" });
    if (!res.ok) {
      budgetLoadError.value = `Refresh failed (${res.status})`;
      return;
    }
    const data = (await res.json()) as { budgets: ResolvedClientBudgets };
    resolvedBudgets.value = data.budgets?.clients ?? [];
  } catch {
    budgetLoadError.value = "Refresh failed";
  } finally {
    budgetRefreshing.value = false;
  }
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

function budgetForClient(clientName: string): ModelContextBudget | undefined {
  return resolvedBudgets.value.find((b) => b.client_name === clientName);
}

function ensureCompaction() {
  if (!local.value.compaction) {
    local.value.compaction = {
      defaults: {},
      model_overrides: {},
      client_overrides: {},
      online_sources: {},
    };
  }
  if (!local.value.compaction.client_overrides) {
    local.value.compaction.client_overrides = {};
  }
}

function getClientBudgetOverride(clientName: string): ModelBudgetOverride {
  ensureCompaction();
  return local.value.compaction?.client_overrides?.[clientName] ?? {};
}

function setClientBudgetOverride(clientName: string, patch: ModelBudgetOverride) {
  ensureCompaction();
  const overrides = { ...(local.value.compaction?.client_overrides ?? {}) };
  const merged = { ...overrides[clientName], ...patch };
  const hasValues = Object.values(merged).some((v) => v !== undefined && v !== null && v !== "");
  if (hasValues) {
    overrides[clientName] = merged;
  } else {
    delete overrides[clientName];
  }
  local.value.compaction = {
    ...local.value.compaction,
    client_overrides: overrides,
  };
  emit("update:modelValue", local.value);
  void loadBudgets();
}

function resetClientBudgetOverride(clientName: string) {
  ensureCompaction();
  const overrides = { ...(local.value.compaction?.client_overrides ?? {}) };
  delete overrides[clientName];
  local.value.compaction = {
    ...local.value.compaction,
    client_overrides: overrides,
  };
  emit("update:modelValue", local.value);
  void loadBudgets();
}

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
    const providerKey =
      typeof defaultProvider === "string" ? defaultProvider : String(defaultProvider);
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

/** Whether any function-level override is set for this agent. */
function hasFnOverrides(agentPkg: string): boolean {
  const af = local.value.overrides?.agent_function ?? {};
  return Object.keys(af).some((k) => k.startsWith(`${agentPkg}:`));
}

// ── Collapsible agent cards ──
// Collapsed by default; badges indicate which agents have overrides.
const expandedAgents = ref<Record<string, boolean>>({});

function toggleAgent(agentPkg: string) {
  const current = expandedAgents.value[agentPkg] ?? false;
  expandedAgents.value = { ...expandedAgents.value, [agentPkg]: !current };
}

function isExpanded(agentPkg: string): boolean {
  return expandedAgents.value[agentPkg] === true;
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

// Validate on every model update so errors are shown live.
watch(
  local,
  () => {
    validationErrors.value = validate();
  },
  { deep: true },
);
</script>

<template>
  <div class="config-llm-form">
    <!-- ══ Clients section ══ -->
    <div class="config-routing-block">
      <div class="config-routing-block-header">
        <div>
          <h3 class="config-section-title">Clients</h3>
          <p class="config-hint">LLM backends available for routing.</p>
        </div>
        <button type="button" class="btn btn--secondary" @click="addClient">Add client</button>
      </div>

      <div class="config-agent-cards">
        <div
          v-for="(names, provider) in clientsByProvider"
          :key="provider"
          class="config-clients-by-provider"
        >
          <h4 class="config-provider-group-title">{{ provider }}</h4>
          <div v-for="name in names" :key="name" class="config-client-card">
            <div class="config-client-header">
              <input
                :value="getClient(name).name"
                class="config-input config-input-inline"
                placeholder="Client name"
                @change="
                  (e) =>
                    setClient(name, {
                      ...getClient(name),
                      name: (e.target as HTMLInputElement).value.trim(),
                    })
                "
              />
              <span v-if="name === defaultClientName" class="config-badge-default">Default</span>
              <button
                v-if="name !== defaultClientName"
                type="button"
                class="btn btn--ghost btn--sm"
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
                @change="
                  (e) =>
                    setClient(name, {
                      ...getClient(name),
                      provider: (e.target as HTMLSelectElement).value,
                    })
                "
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
    </div>

    <!-- ══ Compaction budgets ══ -->
    <div class="config-routing-block">
      <div class="config-routing-block-header">
        <div>
          <h3 class="config-section-title">Compaction budgets</h3>
          <p class="config-hint">
            Model-aware context limits for post-turn and pre-model emergency compaction.
          </p>
        </div>
        <button
          type="button"
          class="btn btn--secondary"
          :disabled="budgetRefreshing"
          @click="refreshBudgetMetadata"
        >
          {{ budgetRefreshing ? "Refreshing…" : "Refresh model metadata" }}
        </button>
      </div>
      <p v-if="budgetLoadError" class="config-error">{{ budgetLoadError }}</p>
      <div class="config-agent-cards">
        <div v-for="name in clientNames" :key="`budget-${name}`" class="config-client-card">
          <div class="config-client-header">
            <strong>{{ name }}</strong>
            <span v-if="budgetForClient(name)" class="config-badge-default">
              {{ budgetForClient(name)?.source }}
            </span>
          </div>
          <template v-if="budgetForClient(name)">
            <p class="config-hint">
              <code>{{ budgetForClient(name)?.model_id }}</code>
              · context {{ formatTokens(budgetForClient(name)!.context_window_tokens) }} tok
              · trigger {{ formatTokens(budgetForClient(name)!.safe_prompt_tokens) }}
              · emergency {{ formatTokens(budgetForClient(name)!.emergency_prompt_tokens) }}
            </p>
            <p v-if="budgetForClient(name)?.warning" class="config-error">
              {{ budgetForClient(name)?.warning }}
            </p>
          </template>
          <div class="config-client-fields">
            <label class="config-label">Context window (tokens)</label>
            <input
              type="number"
              class="config-input"
              :value="getClientBudgetOverride(name).context_window_tokens ?? ''"
              placeholder="Auto"
              @input="
                (e) =>
                  setClientBudgetOverride(name, {
                    context_window_tokens:
                      (e.target as HTMLInputElement).value === ''
                        ? undefined
                        : Number((e.target as HTMLInputElement).value),
                  })
              "
            />
            <label class="config-label">Trigger ratio (0–1)</label>
            <input
              type="number"
              step="0.05"
              min="0.1"
              max="0.95"
              class="config-input"
              :value="getClientBudgetOverride(name).trigger_ratio ?? ''"
              placeholder="Default 0.70"
              @input="
                (e) =>
                  setClientBudgetOverride(name, {
                    trigger_ratio:
                      (e.target as HTMLInputElement).value === ''
                        ? undefined
                        : Number((e.target as HTMLInputElement).value),
                  })
              "
            />
            <label class="config-label">Emergency ratio (0–1)</label>
            <input
              type="number"
              step="0.05"
              min="0.1"
              max="0.99"
              class="config-input"
              :value="getClientBudgetOverride(name).emergency_ratio ?? ''"
              placeholder="Default 0.90"
              @input="
                (e) =>
                  setClientBudgetOverride(name, {
                    emergency_ratio:
                      (e.target as HTMLInputElement).value === ''
                        ? undefined
                        : Number((e.target as HTMLInputElement).value),
                  })
              "
            />
            <button type="button" class="btn btn--ghost btn--sm" @click="resetClientBudgetOverride(name)">
              Reset override
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- ══ Routing section ══ -->
    <div class="config-routing-block">
      <div class="config-routing-block-header">
        <div>
          <h3 class="config-section-title">LLM Routing</h3>
          <p class="config-hint">Controls which client each agent and prompt uses.</p>
        </div>
      </div>

      <!-- System default -->
      <div class="config-override-system-row">
        <div class="config-override-system-label">
          <span
            class="config-label"
            style="font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em"
          >System default</span>
          <span class="config-hint" style="font-size: 11px">All agents inherit this unless overridden</span>
        </div>
        <select v-model="defaultClientName" class="config-input config-select config-select-sm">
          <option v-for="name in clientNames" :key="name" :value="name">{{ name }}</option>
        </select>
      </div>

      <!-- Agent cards -->
      <div class="config-agent-cards">
        <p v-if="discoveredAgents.length === 0" class="config-override-empty">
          No agents discovered — start the runner to populate this section.
        </p>

        <div
          v-for="agent in discoveredAgents"
          :key="agent.agent_package"
          class="config-agent-override-card"
          :class="{ 'is-expanded': isExpanded(agent.agent_package) }"
        >
          <div
            class="config-agent-override-header"
            :class="{ 'is-expandable': functionsForAgent(agent.agent_package).length > 0 }"
            @click="
              functionsForAgent(agent.agent_package).length > 0 && toggleAgent(agent.agent_package)
            "
          >
            <span
              v-if="functionsForAgent(agent.agent_package).length > 0"
              class="config-agent-expand-icon"
              :class="{ 'is-open': isExpanded(agent.agent_package) }"
              aria-hidden="true"
            >▶</span>
            <span v-else class="config-agent-expand-spacer" aria-hidden="true"></span>

            <span class="config-agent-override-name">{{ agent.agent_package }}</span>

            <span
              v-if="hasFnOverrides(agent.agent_package)"
              class="config-agent-fn-badge"
              title="Has per-prompt overrides"
            >prompts overridden</span>

            <div class="config-agent-override-client" @click.stop>
              <span class="config-override-inherit-label">uses</span>
              <select
                :value="getAgentOverride(agent.agent_package)"
                class="config-input config-select config-select-sm"
                :aria-label="'Default client for ' + agent.agent_package"
                @change="
                  (e) =>
                    setAgentOverride(agent.agent_package, (e.target as HTMLSelectElement).value)
                "
              >
                <option value="">{{ local.default }} (system default)</option>
                <option v-for="c in clientNames" :key="c" :value="c">{{ c }}</option>
              </select>
            </div>
          </div>

          <div v-if="isExpanded(agent.agent_package)" class="config-fn-override-list">
            <div
              v-for="fn in functionsForAgent(agent.agent_package)"
              :key="fn"
              class="config-fn-override-row"
            >
              <span class="config-fn-override-name">{{ fn }}</span>
              <div class="config-fn-override-client">
                <select
                  :value="getFnOverride(agent.agent_package, fn)"
                  class="config-input config-select config-select-sm"
                  :aria-label="fn + ' client override'"
                  @change="
                    (e) =>
                      setFnOverride(agent.agent_package, fn, (e.target as HTMLSelectElement).value)
                  "
                >
                  <option value="">
                    {{ agentEffectiveLabel(agent.agent_package) }} (inherited)
                  </option>
                  <option v-for="c in clientNames" :key="c" :value="c">{{ c }}</option>
                </select>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <ul v-if="validationErrors.length > 0" class="config-validation-errors">
      <li v-for="(err, errIdx) in validationErrors" :key="errIdx" class="config-error">
        {{ err }}
      </li>
    </ul>
  </div>
</template>
