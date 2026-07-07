<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import { useToast } from "../composables/useToast";
import type { AgentDiscoveryEntry } from "../types/a2a";
import type {
  EffectiveAgentPolicy,
  EffectiveSystemPolicy,
  SemioticConfig,
  SemioticPolicy,
} from "../types/config";
import { postureChipClass, postureLabel } from "../chat/semioticPolicyUi";
import { readTrustAgentFromUrl } from "../events/operatorRoute";
import SemioticActivityPanel from "./SemioticActivityPanel.vue";
import SemioticPolicyFields from "./SemioticPolicyFields.vue";

const { fetchConfig, putConfig, fetchSemioticEffective } = useConfigApi();
const toast = useToast();
const config = ref<SemioticConfig | null>(null);
const effective = ref<{ system: EffectiveSystemPolicy; agents: EffectiveAgentPolicy[] } | null>(
  null,
);
const discoveredAgents = ref<AgentDiscoveryEntry[]>([]);
const error = ref<string | null>(null);
const saveStatus = ref<"idle" | "saving" | "saved" | "error">("idle");
const version = ref<number>(0);
const expandedAgents = ref<Record<string, boolean>>({});
const agentSearch = ref("");
const highlightAgent = ref<string | null>(null);
let loaded = false;
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

const defaultPolicy = (): SemioticPolicy => ({
  enabled: false,
  mode: "dry_run",
  enforceMinTier: 2,
  requirePostconditionsT3: true,
  strictCitationAnchors: true,
});

function normalizeConfig(raw: Partial<SemioticConfig>): SemioticConfig {
  return {
    ...defaultPolicy(),
    ...raw,
    overrides: {
      agent: { ...(raw.overrides?.agent ?? {}) },
    },
  };
}

function globalPolicy(): SemioticPolicy {
  const c = config.value!;
  return {
    enabled: c.enabled,
    mode: c.mode,
    enforceMinTier: c.enforceMinTier,
    requirePostconditionsT3: c.requirePostconditionsT3,
    strictCitationAnchors: c.strictCitationAnchors,
  };
}

function effectiveForAgent(agentPackage: string): EffectiveAgentPolicy | null {
  return effective.value?.agents.find((a) => a.agentPackage === agentPackage) ?? null;
}

function agentSummaryLabel(agentPackage: string, hasOverride: boolean): string {
  const eff = effectiveForAgent(agentPackage);
  if (!eff) return hasOverride ? "Custom policy" : "Inherits system default";
  return hasOverride ? eff.summary : `Inherits · ${eff.summary}`;
}

function hasAgentOverride(agentPkg: string): boolean {
  return !!config.value?.overrides?.agent?.[agentPkg];
}

function ensureOverrides() {
  if (!config.value) return;
  if (!config.value.overrides) config.value.overrides = { agent: {} };
  if (!config.value.overrides.agent) config.value.overrides.agent = {};
}

function setAgentOverrideEnabled(agentPkg: string, enabled: boolean) {
  if (!config.value) return;
  ensureOverrides();
  const agent = config.value.overrides!.agent!;
  if (enabled) {
    agent[agentPkg] = { ...globalPolicy() };
    expandedAgents.value = { ...expandedAgents.value, [agentPkg]: true };
  } else {
    delete agent[agentPkg];
  }
}

function agentPolicy(agentPkg: string): SemioticPolicy {
  ensureOverrides();
  if (!config.value!.overrides!.agent![agentPkg]) {
    config.value!.overrides!.agent![agentPkg] = { ...globalPolicy() };
  }
  return config.value!.overrides!.agent![agentPkg]!;
}

function toggleAgent(agentPkg: string) {
  expandedAgents.value = {
    ...expandedAgents.value,
    [agentPkg]: !(expandedAgents.value[agentPkg] ?? false),
  };
}

function isExpanded(agentPkg: string): boolean {
  return expandedAgents.value[agentPkg] === true;
}

const filteredAgents = computed(() => {
  const q = agentSearch.value.trim().toLowerCase();
  if (!q) return discoveredAgents.value;
  return discoveredAgents.value.filter((a) => a.agent_package.toLowerCase().includes(q));
});

async function refreshEffective() {
  const result = await fetchSemioticEffective();
  if ("error" in result) return;
  effective.value = { system: result.data.system, agents: result.data.agents };
}

function beforeUnloadHandler(e: BeforeUnloadEvent) {
  if (saveStatus.value === "saving") e.preventDefault();
}

onMounted(async () => {
  window.addEventListener("beforeunload", beforeUnloadHandler);
  highlightAgent.value = readTrustAgentFromUrl();
  if (highlightAgent.value) {
    expandedAgents.value = { [highlightAgent.value]: true };
  }

  try {
    const res = await fetch("/agents");
    if (res.ok) {
      discoveredAgents.value = (await res.json()) as AgentDiscoveryEntry[];
    }
  } catch {
    // non-fatal
  }

  const result = await fetchConfig("semiotic");
  if ("error" in result) {
    error.value = result.error.detail ?? result.error.title;
    return;
  }
  version.value = result.data.version;
  config.value = normalizeConfig(result.data.config as Partial<SemioticConfig>);
  await refreshEffective();
  setTimeout(() => {
    loaded = true;
  }, 0);
});

onUnmounted(() => {
  window.removeEventListener("beforeunload", beforeUnloadHandler);
  if (debounceTimer) clearTimeout(debounceTimer);
});

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

async function doSave(payload: SemioticConfig) {
  const result = await putConfig("semiotic", payload as unknown as Record<string, unknown>, version.value);
  if ("error" in result) {
    saveStatus.value = "error";
    error.value = result.error.detail ?? result.error.title;
    toast.error("Semiotic gate config save failed");
    return;
  }
  version.value = result.data.version;
  saveStatus.value = "saved";
  await refreshEffective();
  toast.success("Semiotic gate config saved");
  setTimeout(() => {
    if (saveStatus.value === "saved") saveStatus.value = "idle";
  }, 2000);
}

const systemPosture = computed(() => effective.value?.system.posture ?? "off");
</script>

<template>
  <div v-if="error" class="settings-error" role="alert">{{ error }}</div>
  <div v-else-if="config" class="config-semiotic-form">
    <div class="config-autosave-bar">
      <span v-if="saveStatus === 'saving'" class="config-autosave-saving">Saving…</span>
      <span v-else-if="saveStatus === 'saved'" class="config-autosave-saved">Saved (v{{ version }})</span>
      <span v-else-if="saveStatus === 'error'" class="config-autosave-error">Save failed</span>
    </div>

    <div>
      <h3 class="config-section-title">Semiotic gate</h3>
      <p class="config-semiotic-lede">
        Pre-action structural gate for tier&ge;2 tool calls. Dry-run records decisions without blocking.
        Per-agent overrides inherit the system default unless customized below.
        <a
          class="config-semiotic-doc-link"
          href="https://github.com/semiotic-agentium/agentium-os/blob/main/docs/assertions/semiotic-gate.md"
          target="_blank"
          rel="noopener noreferrer"
        >How grounding works</a>
      </p>
    </div>

    <div class="config-routing-block">
      <div class="config-routing-block-header">
        <div>
          <h3 class="config-section-title">System default</h3>
          <p class="config-hint">All agents inherit this unless overridden.</p>
        </div>
        <span :class="postureChipClass(systemPosture)">{{ postureLabel(systemPosture) }}</span>
      </div>
      <div class="config-routing-block-body">
        <SemioticPolicyFields :policy="config" />
      </div>
    </div>

    <div class="config-routing-block">
      <div class="config-routing-block-header">
        <div>
          <h3 class="config-section-title">Per-agent policy</h3>
          <p class="config-hint">Keyed by agent package (same as LLM routing).</p>
        </div>
      </div>
      <div class="config-routing-block-body">
        <input
          v-model="agentSearch"
          class="config-input semiotic-agent-search"
          type="search"
          placeholder="Filter agents…"
          aria-label="Filter agents"
        />
        <div class="config-agent-cards">
          <p v-if="discoveredAgents.length === 0" class="config-override-empty">
            No agents discovered — deploy an agent to configure per-package trust policy.
          </p>
          <p v-else-if="filteredAgents.length === 0" class="config-override-empty">
            No agents match filter.
          </p>
          <div
            v-for="agent in filteredAgents"
            :key="agent.agent_package"
            class="config-agent-override-card"
            :class="{
              'is-expanded': isExpanded(agent.agent_package),
              'is-highlighted': highlightAgent === agent.agent_package,
            }"
          >
            <div
              class="config-agent-override-header"
              :class="{ 'is-expandable': hasAgentOverride(agent.agent_package) }"
              @click="hasAgentOverride(agent.agent_package) && toggleAgent(agent.agent_package)"
            >
              <span
                v-if="hasAgentOverride(agent.agent_package)"
                class="config-agent-chevron"
                :class="{ 'is-open': isExpanded(agent.agent_package) }"
                aria-hidden="true"
              />
              <span class="config-agent-override-name">{{ agent.agent_package }}</span>
              <span class="config-agent-summary">{{
                agentSummaryLabel(agent.agent_package, hasAgentOverride(agent.agent_package))
              }}</span>
              <span v-if="hasAgentOverride(agent.agent_package)" class="tool-bundle-badge">Custom</span>
              <label class="config-field config-checkbox" @click.stop>
                <input
                  :checked="hasAgentOverride(agent.agent_package)"
                  type="checkbox"
                  @change="
                    setAgentOverrideEnabled(
                      agent.agent_package,
                      ($event.target as HTMLInputElement).checked,
                    )
                  "
                />
                <span>Custom policy</span>
              </label>
            </div>
            <div
              v-if="hasAgentOverride(agent.agent_package) && isExpanded(agent.agent_package)"
              class="config-agent-override-body config-routing-block-body"
            >
              <SemioticPolicyFields :policy="agentPolicy(agent.agent_package)" />
            </div>
          </div>
        </div>
      </div>
    </div>

    <SemioticActivityPanel :search-filter="agentSearch.trim() || null" />
  </div>
</template>
