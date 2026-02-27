<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import type { AgentDiscoveryEntry } from "../types/a2a";

// Static agent capability registry derived from tests/fixtures/agents/
const AGENT_REGISTRY = [
  {
    id: "task-lifecycle-demo",
    tags: ["Multi-turn", "FSM", "Artifacts"],
    description: "Full lifecycle with review and sign-off loops",
  },
  {
    id: "conversational-persona-demo",
    tags: ["A2A", "BAML", "Multi-agent", "Persona"],
    description: "Orkemedies persona with agent discovery and A2A delegation",
  },
  {
    id: "stream-baml-tool",
    tags: ["BAML", "Streaming", "Tools"],
    description: "Streams ChooseCalcTool results via sendStream",
  },
  {
    id: "stream-js-tool",
    tags: ["Streaming", "Artifacts"],
    description: "Pure JS streaming with artifact emission",
  },
  {
    id: "conversational-context-auto",
    tags: ["BAML", "Context", "Auto-routing"],
    description: "Auto-context with compute vs chat routing",
  },
  {
    id: "argument-chapman",
    tags: ["Multi-turn", "A2A"],
    description: "Monty Python argument responder (two-turn)",
  },
  {
    id: "argument-cleese",
    tags: ["Multi-turn", "A2A", "Orchestration"],
    description: "Argument initiator with A2A routing to Chapman",
  },
  {
    id: "memory-smoke-tool",
    tags: ["Memory", "Tools", "Graph"],
    description: "Memory graph ops: add, link, search, traverse",
  },
  {
    id: "tool-discovery-demo",
    tags: ["Tools", "Discovery", "Dynamic"],
    description: "Dynamic tool discovery via system/discover_tools",
  },
];

const props = defineProps<{ agents: AgentDiscoveryEntry[] }>();

// Merge live API agents with static registry
const agentRows = computed(() => {
  const activeMap = new Map(props.agents.map((a) => [a.agent_package, a]));
  return AGENT_REGISTRY.map((r) => ({
    ...r,
    active: activeMap.has(r.id),
    version: activeMap.get(r.id)?.version ?? "—",
  }));
});

const totalFixtures = AGENT_REGISTRY.length;
const activeCount = computed(() => agentRows.value.filter((a) => a.active).length);

// Last sync: time when component mounted (agents just fetched)
const lastSyncTime = ref("");
const lastSyncAgo = ref("—");
onMounted(() => {
  const now = new Date();
  lastSyncTime.value = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  lastSyncAgo.value = "just now";
});

// Mock success rate
const successRate = "98.6%";

// Sparkline — mock reasoning latency samples (ms)
const LATENCY = [48, 62, 45, 71, 58, 43, 67, 52, 38, 61, 55, 47, 73, 49, 65];
const SPARK_W = 320;
const SPARK_H = 56;

const sparkPoints = computed(() => {
  const max = Math.max(...LATENCY);
  const min = Math.min(...LATENCY);
  const range = max - min || 1;
  const pad = 4;
  return LATENCY.map((v, i) => {
    const x = ((i / (LATENCY.length - 1)) * SPARK_W).toFixed(1);
    const y = (pad + ((max - v) / range) * (SPARK_H - pad * 2)).toFixed(1);
    return `${x},${y}`;
  }).join(" ");
});

const sparkFillPath = computed(() => {
  const coords = sparkPoints.value.split(" ");
  return `M ${coords.join(" L ")} L ${SPARK_W},${SPARK_H} L 0,${SPARK_H} Z`;
});

const currentLatency = LATENCY[LATENCY.length - 1];
const avgLatency = Math.round(LATENCY.reduce((a, b) => a + b) / LATENCY.length);
const p99Latency = Math.max(...LATENCY);

function tagClass(tag: string): string {
  const t = tag.toLowerCase();
  if (["baml", "context", "auto-routing", "persona"].includes(t)) return "tag-baml";
  if (t === "streaming") return "tag-streaming";
  if (["multi-turn", "fsm", "orchestration"].includes(t)) return "tag-multiturn";
  if (["a2a", "multi-agent"].includes(t)) return "tag-a2a";
  if (["tools", "discovery", "dynamic"].includes(t)) return "tag-tools";
  if (["memory", "graph"].includes(t)) return "tag-memory";
  if (t === "artifacts") return "tag-artifacts";
  return "tag-default";
}
</script>

<template>
  <div class="dashboard">
    <!-- ── Top row: 3 stat cards ── -->
    <div class="dashboard-grid-top">
      <!-- Total Fixtures -->
      <div class="stat-card">
        <div class="stat-card-label">
          <!-- Package icon -->
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z" />
            <polyline points="3.27 6.96 12 12.01 20.73 6.96" />
            <line x1="12" y1="22.08" x2="12" y2="12" />
          </svg>
          Total Fixtures
        </div>
        <div class="stat-card-value">{{ totalFixtures }}</div>
        <div class="stat-card-sub">{{ activeCount }} currently active</div>
      </div>

      <!-- Last Sync -->
      <div class="stat-card">
        <div class="stat-card-label">
          <!-- Clock icon -->
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="10" />
            <polyline points="12 6 12 12 16 14" />
          </svg>
          Last Sync
        </div>
        <div class="stat-card-value" style="font-size: 22px; letter-spacing: -0.01em;">{{ lastSyncTime }}</div>
        <div class="stat-card-sub">{{ lastSyncAgo }}</div>
      </div>

      <!-- Success Rate -->
      <div class="stat-card">
        <div class="stat-card-label">
          <!-- Activity icon -->
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
          </svg>
          Success Rate
        </div>
        <div class="stat-card-value success-value">{{ successRate }}</div>
        <div class="stat-card-sub">Last 24 h · all tasks</div>
      </div>
    </div>

    <!-- ── Bottom row: agent table + system health ── -->
    <div class="dashboard-grid-bottom">
      <!-- Agent Inventory Table -->
      <div class="dashboard-card">
        <div class="dashboard-card-header">
          <!-- List icon -->
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <line x1="8" y1="6" x2="21" y2="6" /><line x1="8" y1="12" x2="21" y2="12" /><line x1="8" y1="18" x2="21" y2="18" />
            <line x1="3" y1="6" x2="3.01" y2="6" /><line x1="3" y1="12" x2="3.01" y2="12" /><line x1="3" y1="18" x2="3.01" y2="18" />
          </svg>
          Agent Inventory
        </div>
        <div class="dashboard-card-body">
          <table class="agent-table">
            <thead>
              <tr>
                <th></th>
                <th>Agent</th>
                <th>Capabilities</th>
                <th>Version</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="agent in agentRows" :key="agent.id">
                <td style="width: 32px; text-align: center;">
                  <span :class="['agent-status-dot', agent.active ? 'dot-active' : 'dot-idle']" />
                </td>
                <td>
                  <div class="agent-name">{{ agent.id }}</div>
                  <div class="agent-desc">{{ agent.description }}</div>
                </td>
                <td>
                  <div class="agent-tags">
                    <span v-for="tag in agent.tags" :key="tag" :class="['agent-tag', tagClass(tag)]">
                      {{ tag }}
                    </span>
                  </div>
                </td>
                <td class="agent-version">{{ agent.version }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <!-- System Health: Reasoning Latency Sparkline -->
      <div class="dashboard-card">
        <div class="dashboard-card-header">
          <!-- Pulse icon -->
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
          </svg>
          System Health
        </div>
        <div class="sparkline-card-body">
          <div>
            <div class="stat-card-label" style="margin-bottom: 6px;">Reasoning Latency</div>
            <div class="sparkline-current">
              <span class="sparkline-value">{{ currentLatency }}</span>
              <span class="sparkline-unit">ms</span>
            </div>
          </div>

          <!-- SVG sparkline -->
          <svg class="sparkline-svg" :viewBox="`0 0 ${SPARK_W} ${SPARK_H}`" preserveAspectRatio="none">
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
              <span class="sparkline-status-key">Current</span>
              <span class="sparkline-status-val">{{ currentLatency }} ms</span>
            </div>
            <div class="sparkline-status-item">
              <span class="sparkline-status-key">Avg</span>
              <span class="sparkline-status-val">{{ avgLatency }} ms</span>
            </div>
            <div class="sparkline-status-item">
              <span class="sparkline-status-key">P99</span>
              <span class="sparkline-status-val">{{ p99Latency }} ms</span>
            </div>
            <div class="sparkline-status-item">
              <span class="sparkline-status-key">Status</span>
              <span class="sparkline-status-val" style="color: var(--status-green);">Nominal</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
