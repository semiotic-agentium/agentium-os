<script setup lang="ts">
import type { SystemCapacityRow } from "../../composables/useDashboardViewModel";

defineProps<{
  agentRows: SystemCapacityRow[];
  runnerOnline: boolean;
}>();

const emit = defineEmits<{
  "open-settings": [];
}>();

function tagClass(tag: string): string {
  const t = tag.toLowerCase();
  if (["baml", "context", "auto-routing", "persona"].includes(t)) return "tag-baml";
  if (t === "streaming") return "tag-streaming";
  if (["multi-turn", "fsm", "orchestration"].includes(t)) return "tag-multiturn";
  if (["a2a", "multi-agent", "delegation"].includes(t)) return "tag-a2a";
  if (["tools", "discovery", "dynamic", "tool_use"].includes(t)) return "tag-tools";
  if (["memory", "graph"].includes(t)) return "tag-memory";
  if (t === "artifacts") return "tag-artifacts";
  if (t.startsWith("system/")) return "tag-a2a";
  if (t.startsWith("support/")) return "tag-tools";
  if (t.includes("memory")) return "tag-memory";
  if (t.includes("stream")) return "tag-streaming";
  return "tag-default";
}

function shortToolName(tool: string): string {
  const parts = tool.split("/");
  return parts[parts.length - 1] ?? tool;
}
</script>

<template>
  <section class="dashboard-narrative-section" aria-labelledby="dash-system-heading">
    <div class="dashboard-narrative-head">
      <h2 id="dash-system-heading" class="dashboard-narrative-title">System surface</h2>
      <p class="dashboard-narrative-lede">
        Deployed agents and runner reachability. Configuration is an action — not a headline metric.
      </p>
    </div>

    <div class="dashboard-system-bar">
      <div class="dashboard-system-pill" :data-online="runnerOnline">
        <span class="dashboard-system-dot" />
        {{ runnerOnline ? "Runner reachable" : "Runner unreachable" }}
      </div>
      <button type="button" class="btn-primary-soft" @click="emit('open-settings')">
        Configuration
      </button>
    </div>

    <div class="dashboard-card">
      <div class="dashboard-card-header">Agent inventory</div>
      <div class="dashboard-card-body">
        <table class="agent-table">
          <thead>
            <tr>
              <th></th>
              <th>Agent</th>
              <th>Tools &amp; capabilities</th>
              <th>Version</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="agent in agentRows" :key="agent.id">
              <td style="width: 32px; text-align: center">
                <span :class="['agent-status-dot', agent.discovered ? 'dot-active' : 'dot-idle']"></span>
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
            <tr v-if="agentRows.length === 0">
              <td colspan="4" class="dashboard-table-empty">
                No agents discovered — start the runner and open Chat to refresh discovery.
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
</template>
