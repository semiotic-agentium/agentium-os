<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import type { AgentDiscoveryEntry, ChatMessage, ContextMetricsResponse } from "../types/a2a";
import type { ContextPlanningResponse, ProvenanceGroupHotspot } from "../types/provenance";
import { useMermaidRenderer } from "../composables/useMermaidRenderer";
import { useTheme } from "../composables/useTheme";
import {
  formatCompact as formatTokenCount,
  formatDuration,
  formatKb,
  normalizeGroupValue,
  asDisplayIdentity,
} from "../utils/format";
import InterpretationPanel from "./InterpretationPanel.vue";

const props = defineProps<{
  agents: AgentDiscoveryEntry[];
  contextMetrics: ContextMetricsResponse | null;
  /** From conversation-history SSE: temporal tail of serialized prompt JSON bytes */
  promptContextBytesSessionCurrent?: number | null;
  provenanceDiagram: string;
  messages: ChatMessage[];
  contextId?: string;
  provenanceSummary?: {
    count: number;
    failedCount: number;
    durationMsTotal: number;
    totalTokens: number;
    llmCount: number;
    toolCount: number;
    hotspotGroups: ProvenanceGroupHotspot[];
    lastUpdatedAt: number;
  } | null;
}>();

const emit = defineEmits<{ "open-settings": [] }>();

// ── Planning state ──
const planningData = ref<ContextPlanningResponse | null>(null);

watch(
  () => props.contextId,
  async (ctxId) => {
    if (!ctxId) {
      planningData.value = null;
      return;
    }
    try {
      const res = await fetch(`/contexts/${ctxId}/planning`);
      if (res.ok) planningData.value = await res.json();
    } catch {
      // planning endpoint may not be available
    }
  },
  { immediate: true },
);

const planningStatus = computed(() => {
  const tasks = planningData.value?.tasks ?? [];
  if (tasks.length === 0) return null;
  const task = tasks[0]!;
  const intent = task.currentIntent?.description ?? null;
  const summary = task.stepSummary;
  const total = summary?.total ?? 0;
  const completed = summary?.completed ?? 0;
  const driftSeverity = task.drift?.compositeSeverity ?? null;
  return { intent, total, completed, driftSeverity };
});

const agentRows = computed(() =>
  props.agents.map((a) => ({
    id: a.agent_package,
    name: a.agent_card?.name ?? a.name,
    description: a.agent_card?.description ?? null,
    version: a.version,
    tools: a.agent_card?.tools ?? [],
    capabilities: a.agent_card?.capabilities ?? [],
    discovered: !!a.agent_card,
  })),
);

const agentCount = computed(() => agentRows.value.length);

// Last sync: updates relative time every 30s
const lastSyncTimestamp = ref(0);
const lastSyncAgo = ref("—");
let syncTimer: ReturnType<typeof setInterval> | null = null;

function updateSyncAgo() {
  if (!lastSyncTimestamp.value) return;
  const elapsed = Math.floor((Date.now() - lastSyncTimestamp.value) / 1000);
  if (elapsed < 10) lastSyncAgo.value = "just now";
  else if (elapsed < 60) lastSyncAgo.value = `${elapsed}s ago`;
  else lastSyncAgo.value = `${Math.floor(elapsed / 60)}m ago`;
}

onMounted(() => {
  lastSyncTimestamp.value = Date.now();
  updateSyncAgo();
  syncTimer = setInterval(updateSyncAgo, 30_000);
});

onUnmounted(() => {
  if (syncTimer) clearInterval(syncTimer);
});

// ── Token metrics (from context metrics API) ──

const SPARK_W = 320;
const SPARK_H = 56;

const totalTokens = computed(() => props.contextMetrics?.session.tokens_total.total ?? null);
const avgLatencyMs = computed(() => {
  if (!props.contextMetrics) return null;
  const { llm_calls_total, llm_duration_ms_total } = props.contextMetrics.session;
  if (llm_calls_total === 0) return 0;
  return Math.round(llm_duration_ms_total / llm_calls_total);
});

/** Prefer live SSE tail when hydrated; else metrics API temporal tail. */
const sessionPromptBytesCurrent = computed(() => {
  const sse = props.promptContextBytesSessionCurrent;
  if (sse != null) return sse;
  return props.contextMetrics?.session.prompt_context_bytes_current ?? null;
});

const provenanceSuccessRate = computed(() => {
  const total = props.provenanceSummary?.count ?? 0;
  if (total <= 0) return null;
  const failed = props.provenanceSummary?.failedCount ?? 0;
  const successful = Math.max(0, total - failed);
  return Math.round((successful / total) * 100);
});

const turnTokens = computed(() => {
  if (!props.contextMetrics) return [];
  return props.contextMetrics.turns.map((t) => t.tokens.total);
});

const sparkPoints = computed(() => {
  const data = turnTokens.value;
  if (data.length < 2) return "";
  const max = Math.max(...data);
  const min = Math.min(...data);
  const range = max - min || 1;
  const pad = 4;
  return data
    .map((v, i) => {
      const x = ((i / (data.length - 1)) * SPARK_W).toFixed(1);
      const y = (pad + ((max - v) / range) * (SPARK_H - pad * 2)).toFixed(1);
      return `${x},${y}`;
    })
    .join(" ");
});

const sparkFillPath = computed(() => {
  if (!sparkPoints.value) return "";
  const coords = sparkPoints.value.split(" ");
  return `M ${coords.join(" L ")} L ${SPARK_W},${SPARK_H} L 0,${SPARK_H} Z`;
});


function groupDimensionValue(group: ProvenanceGroupHotspot, dimension: string): string | undefined {
  const dimensions = Array.isArray(group.groupDimensions) ? group.groupDimensions : [];
  const values = Array.isArray(group.groupValues) ? group.groupValues : [];
  const idx = dimensions.indexOf(dimension);
  if (idx >= 0) return normalizeGroupValue(values[idx]);
  // Fallback for older payloads where only pipe-encoded groupKey is present.
  const legacyValues = group.groupKey.split("|");
  const legacyIdx =
    dimension === "agent_id"
      ? 0
      : dimension === "agent_package"
        ? 1
        : dimension === "agent_version"
          ? 2
          : dimension === "model" || dimension === "tool_name"
            ? 3
            : -1;
  if (legacyIdx >= 0) return normalizeGroupValue(legacyValues[legacyIdx]);
  return undefined;
}

function hotspotLabel(group: ProvenanceGroupHotspot): string {
  const agentDisplay = asDisplayIdentity(
    groupDimensionValue(group, "agent_id"),
    groupDimensionValue(group, "agent_package"),
    groupDimensionValue(group, "agent_version"),
  );
  const model = groupDimensionValue(group, "model");
  const toolName = groupDimensionValue(group, "tool_name");
  if (model) return `${agentDisplay} · ${model}`;
  if (toolName) return `${agentDisplay} · ${toolName}`;
  return agentDisplay;
}

// ── Provenance diagram preview ──

const { theme } = useTheme();
const diagramSources = computed(() => (props.provenanceDiagram ? [props.provenanceDiagram] : []));
const { rendered: renderedDiagrams } = useMermaidRenderer(diagramSources, theme);
const expandedDiagram = ref(false);

// ── Tag styling ──

function tagClass(tag: string): string {
  const t = tag.toLowerCase();
  if (["baml", "context", "auto-routing", "persona"].includes(t)) return "tag-baml";
  if (t === "streaming") return "tag-streaming";
  if (["multi-turn", "fsm", "orchestration"].includes(t)) return "tag-multiturn";
  if (["a2a", "multi-agent", "delegation"].includes(t)) return "tag-a2a";
  if (["tools", "discovery", "dynamic", "tool_use"].includes(t)) return "tag-tools";
  if (["memory", "graph"].includes(t)) return "tag-memory";
  if (t === "artifacts") return "tag-artifacts";
  // Tool name patterns from agent_card.tools
  if (t.startsWith("system/")) return "tag-a2a";
  if (t.startsWith("support/")) return "tag-tools";
  if (t.includes("memory")) return "tag-memory";
  if (t.includes("stream")) return "tag-streaming";
  return "tag-default";
}

/** Show short name for namespaced tools (e.g. "support/calculate" → "calculate") */
function shortToolName(tool: string): string {
  const parts = tool.split("/");
  return parts[parts.length - 1] ?? tool;
}
</script>

<template>
  <div class="dashboard">
    <!-- ── Top row: 3 stat cards ── -->
    <div class="dashboard-grid-top">
      <!-- Active Agents -->
      <div class="stat-card">
        <div class="stat-card-label">
          <!-- Package icon -->
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path
              d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"
            />
            <polyline points="3.27 6.96 12 12.01 20.73 6.96" />
            <line x1="12" y1="22.08" x2="12" y2="12" />
          </svg>
          Active Agents
        </div>
        <div class="stat-card-value">{{ agentCount }}</div>
        <div class="stat-card-sub">{{ agentCount }} discovered via API</div>
      </div>

      <!-- Last Sync -->
      <div class="stat-card">
        <div class="stat-card-label">
          <!-- Clock icon -->
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <circle cx="12" cy="12" r="10" />
            <polyline points="12 6 12 12 16 14" />
          </svg>
          Last Sync
        </div>
        <div class="stat-card-value stat-card-value--sm">{{ lastSyncTimestamp ? new Date(lastSyncTimestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '—' }}</div>
        <div class="stat-card-sub">{{ lastSyncAgo }}</div>
      </div>

      <!-- Prompt JSON size (current invocation tail, not peak) -->
      <div class="stat-card">
        <div class="stat-card-label">
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
            <polyline points="14 2 14 8 20 8" />
          </svg>
          Prompt JSON (current)
        </div>
        <div v-if="sessionPromptBytesCurrent != null" class="stat-card-value stat-card-value--sm">
          {{ formatKb(sessionPromptBytesCurrent) }}
        </div>
        <div v-else class="stat-card-value" style="color: var(--text-muted)">&mdash;</div>
        <div class="stat-card-sub">Latest LLM call prompt JSON (UTF-8)</div>
      </div>

      <!-- Token Usage -->
      <div class="stat-card">
        <div class="stat-card-label">
          <!-- Coins icon -->
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <circle cx="8" cy="8" r="6" />
            <path d="M18.09 10.37A6 6 0 1 1 10.34 18" />
            <path d="M7 6h1v4" />
          </svg>
          Token Usage
        </div>
        <div v-if="totalTokens !== null" class="stat-card-value">
          {{ formatTokenCount(totalTokens) }}
        </div>
        <div v-else class="stat-card-value" style="color: var(--text-muted)">&mdash;</div>
        <div v-if="contextMetrics" class="stat-card-sub">
          {{ formatTokenCount(contextMetrics.session.tokens_total.in) }} in /
          {{ formatTokenCount(contextMetrics.session.tokens_total.out) }} out
        </div>
        <div v-else class="stat-card-sub">No conversation data</div>
      </div>

      <!-- Provenance Success -->
      <div class="stat-card">
        <div class="stat-card-label">
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M20 6L9 17l-5-5" />
          </svg>
          Provenance Success
        </div>
        <div v-if="provenanceSuccessRate !== null" class="stat-card-value success-value">
          {{ provenanceSuccessRate }}%
        </div>
        <div v-else class="stat-card-value" style="color: var(--text-muted)">&mdash;</div>
        <div v-if="provenanceSummary" class="stat-card-sub">
          {{ provenanceSummary.count }} ops · {{ provenanceSummary.failedCount }} failed
        </div>
        <div v-else class="stat-card-sub">No provenance data</div>
      </div>

      <!-- Configuration -->
      <button type="button" class="stat-card stat-card-button" @click="emit('open-settings')">
        <div class="stat-card-label">
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <circle cx="12" cy="12" r="3" />
            <path
              d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"
            />
          </svg>
          Configuration
        </div>
        <div class="stat-card-value stat-card-value--label">LLM &amp; Tools</div>
        <div class="stat-card-sub">Configure clients and tool bundles</div>
      </button>

      <!-- Planning Status -->
      <div v-if="planningStatus" class="stat-card">
        <div class="stat-card-label">
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
            <polyline points="14 2 14 8 20 8" />
            <line x1="16" y1="13" x2="8" y2="13" />
            <line x1="16" y1="17" x2="8" y2="17" />
          </svg>
          Agent Plan
        </div>
        <div class="stat-card-value stat-card-value--sm">{{ planningStatus.completed }}/{{ planningStatus.total }} steps</div>
        <div class="stat-card-sub">
          <span v-if="planningStatus.driftSeverity" :class="['planning-drift-label', `drift-${planningStatus.driftSeverity}`]">
            Drift: {{ planningStatus.driftSeverity }}
          </span>
          <span v-else>On track</span>
        </div>
      </div>
    </div>

    <!-- ── Bottom row: agent table + session metrics + interpretation ── -->
    <div class="dashboard-grid-bottom">
      <!-- Agent Inventory Table -->
      <div class="dashboard-card">
        <div class="dashboard-card-header">
          <!-- List icon -->
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <line x1="8" y1="6" x2="21" y2="6" />
            <line x1="8" y1="12" x2="21" y2="12" />
            <line x1="8" y1="18" x2="21" y2="18" />
            <line x1="3" y1="6" x2="3.01" y2="6" />
            <line x1="3" y1="12" x2="3.01" y2="12" />
            <line x1="3" y1="18" x2="3.01" y2="18" />
          </svg>
          Agent Inventory
        </div>
        <div class="dashboard-card-body">
          <table class="agent-table">
            <thead>
              <tr>
                <th></th>
                <th>Agent</th>
                <th>Tools &amp; Capabilities</th>
                <th>Version</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="agent in agentRows" :key="agent.id">
                <td style="width: 32px; text-align: center">
                  <span :class="['agent-status-dot', agent.discovered ? 'dot-active' : 'dot-stale']"></span>
                </td>
                <td>
                  <div class="agent-name">{{ agent.name }}</div>
                  <div class="agent-desc">{{ agent.description ?? "—" }}</div>
                </td>
                <td>
                  <div class="agent-tags">
                    <span
                      v-for="cap in agent.capabilities"
                      :key="'cap-' + cap"
                      :class="['agent-tag', tagClass(cap)]"
                    >
                      {{ cap }}
                    </span>
                    <span
                      v-for="tool in agent.tools"
                      :key="'tool-' + tool"
                      :class="['agent-tag', tagClass(tool)]"
                    >
                      {{ shortToolName(tool) }}
                    </span>
                  </div>
                </td>
                <td class="agent-version">{{ agent.version }}</td>
              </tr>
              <!-- Empty state -->
              <tr v-if="agentRows.length === 0">
                <td
                  colspan="4"
                  style="text-align: center; padding: 32px 16px; color: var(--text-muted)"
                >
                  No agents discovered — start the runner to populate this table
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <!-- Session Metrics + Provenance Preview -->
      <div class="dashboard-card">
        <div class="dashboard-card-header">
          <!-- Pulse icon -->
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
          </svg>
          Session Metrics
        </div>
        <div class="sparkline-card-body">
          <!-- Metrics section (when data available) -->
          <template v-if="contextMetrics">
            <div>
              <div class="stat-card-label" style="margin-bottom: 6px">Token Usage Per Turn</div>
              <div class="sparkline-current">
                <span class="sparkline-value">{{
                  formatTokenCount(contextMetrics.session.tokens_total.total)
                }}</span>
                <span class="sparkline-unit">total</span>
              </div>
            </div>

            <!-- Sparkline (from real per-turn data) -->
            <svg
              v-if="turnTokens.length >= 2"
              class="sparkline-svg"
              :viewBox="`0 0 ${SPARK_W} ${SPARK_H}`"
              preserveAspectRatio="none"
              role="img"
              :aria-label="`Token usage per turn: ${formatTokenCount(totalTokens ?? 0)} total across ${turnTokens.length} turns`"
            >
              <defs>
                <linearGradient id="sparkGrad" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stop-color="var(--primary)" stop-opacity="0.3" />
                  <stop offset="100%" stop-color="var(--primary)" stop-opacity="0" />
                </linearGradient>
              </defs>
              <path :d="sparkFillPath" fill="url(#sparkGrad)" />
              <polyline
                :points="sparkPoints"
                fill="none"
                stroke="var(--primary)"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>

            <!-- Stats grid -->
            <div class="sparkline-status-grid">
              <div class="sparkline-status-item">
                <span class="sparkline-status-key">LLM Calls</span>
                <span class="sparkline-status-val">{{
                  contextMetrics.session.llm_calls_total
                }}</span>
              </div>
              <div class="sparkline-status-item">
                <span class="sparkline-status-key">Avg Latency</span>
                <span class="sparkline-status-val">{{ avgLatencyMs ?? "—" }} ms</span>
              </div>
              <div class="sparkline-status-item">
                <span class="sparkline-status-key">Turns</span>
                <span class="sparkline-status-val">{{ contextMetrics.session.turns_total }}</span>
              </div>
              <div class="sparkline-status-item">
                <span class="sparkline-status-key">Tokens In</span>
                <span class="sparkline-status-val">{{
                  formatTokenCount(contextMetrics.session.tokens_total.in)
                }}</span>
              </div>
              <div class="sparkline-status-item">
                <span class="sparkline-status-key">Prompt JSON</span>
                <span class="sparkline-status-val">{{
                  sessionPromptBytesCurrent != null ? formatKb(sessionPromptBytesCurrent) : "—"
                }}</span>
              </div>
              <div class="sparkline-status-item">
                <span class="sparkline-status-key">Prov Duration</span>
                <span class="sparkline-status-val">{{
                  provenanceSummary ? formatDuration(provenanceSummary.durationMsTotal) : "—"
                }}</span>
              </div>
            </div>

            <div
              v-if="provenanceSummary && provenanceSummary.hotspotGroups.length > 0"
              class="dashboard-hotspots"
            >
              <div class="stat-card-label">Top Hotspots</div>
              <ul>
                <li
                  v-for="group in provenanceSummary.hotspotGroups.slice(0, 3)"
                  :key="group.groupKey"
                >
                  <span class="group-key">{{ hotspotLabel(group) }}</span>
                  <span>{{ group.count }} · {{ formatDuration(Math.round(group.avgDurationMs)) }}</span>
                </li>
              </ul>
            </div>
          </template>

          <!-- Empty state -->
          <div v-else class="empty-state" style="flex: 1">
            <svg
              class="empty-state-icon"
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
            </svg>
            <span class="empty-state-text">Run a conversation to see session metrics</span>
          </div>

          <!-- Provenance preview (when diagram available) -->
          <div
            v-if="renderedDiagrams.length > 0 && renderedDiagrams[0] && !renderedDiagrams[0].error"
            class="provenance-preview"
          >
            <div class="stat-card-label" style="margin-bottom: 6px">Last Trace</div>
            <div
              class="provenance-miniature"
              title="Click to expand"
              @click="expandedDiagram = true"
            >
              <!-- eslint-disable-next-line vue/no-v-html -->
              <div class="diagram-svg" v-html="renderedDiagrams[0].svg"></div>
              <div class="diagram-expand-hint">
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <polyline points="15 3 21 3 21 9" />
                  <polyline points="9 21 3 21 3 15" />
                  <line x1="21" y1="3" x2="14" y2="10" />
                  <line x1="3" y1="21" x2="10" y2="14" />
                </svg>
              </div>
            </div>
          </div>
        </div>
      </div>
      <!-- Interpretation Panel -->
      <InterpretationPanel :messages="messages" />
    </div>
    <!-- Provenance diagram modal -->
    <Teleport to="body">
      <div
        v-if="expandedDiagram && renderedDiagrams[0] && !renderedDiagrams[0].error"
        class="diagram-modal-overlay"
        tabindex="-1"
        @click.self="expandedDiagram = false"
        @keydown.escape="expandedDiagram = false"
      >
        <div class="diagram-modal" role="dialog" aria-modal="true" aria-label="Provenance diagram">
          <header class="diagram-modal-header">
            <span class="diagram-modal-title">Provenance Trace</span>
            <button
              class="diagram-modal-btn diagram-modal-close"
              title="Close"
              @click="expandedDiagram = false"
            >
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              >
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </header>
          <!-- eslint-disable-next-line vue/no-v-html -->
          <div class="diagram-modal-body" v-html="renderedDiagrams[0].svg"></div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
