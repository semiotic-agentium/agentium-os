<script setup lang="ts">
import { ref, watch, nextTick, computed } from "vue";
import type { ToolEvent, ToolNotificationBlock } from "../types/a2a";

const props = defineProps<{ block: ToolNotificationBlock }>();

const bodyEl = ref<HTMLElement | null>(null);
const userScrolledUp = ref(false);
const scrollThreshold = 60;

function toolUseSummary(ev: ToolEvent): { name: string; detail: string } | null {
  if (ev.kind !== "assistant_tool_use") return null;
  const name = (ev.name ?? "tool") as string;
  let detail = "";
  try {
    const input = ev.input;
    if (typeof input === "string") {
      const parsed = JSON.parse(input) as Record<string, unknown>;
      if (typeof parsed.file_path === "string") {
        const file = parsed.file_path.split("/").pop() ?? parsed.file_path;
        detail = file;
      } else if (typeof parsed.description === "string") {
        detail = parsed.description;
      } else if (typeof parsed.command === "string") {
        detail = parsed.command;
      }
    } else if (input && typeof input === "object" && !Array.isArray(input)) {
      const o = input as Record<string, unknown>;
      if (typeof o.file_path === "string") detail = (o.file_path as string).split("/").pop() ?? o.file_path;
      else if (typeof o.description === "string") detail = o.description as string;
      else if (typeof o.command === "string") detail = o.command as string;
    }
  } catch {
    /* ignore parse errors */
  }
  return { name, detail };
}

function eventDisplay(ev: ToolEvent): { kind: string; text: string; toolUse?: { name: string; detail: string } } {
  if (ev.kind === "assistant_thinking" && typeof ev.thinking === "string") {
    return { kind: "thinking", text: ev.thinking.trim() };
  }
  if (ev.kind === "assistant_text" && typeof ev.text === "string") {
    return { kind: "text", text: ev.text.trim() };
  }
  if (ev.kind === "assistant_tool_use") {
    const summary = toolUseSummary(ev);
    return {
      kind: "tool_use",
      text: summary ? (summary.detail ? `${summary.name}: ${summary.detail}` : summary.name) : (ev.name ?? "tool"),
      toolUse: summary ?? undefined,
    };
  }
  if (ev.kind === "terminal_result") {
    const sub = ev.subtype ?? "done";
    return { kind: "terminal", text: sub === "success" ? "Complete" : sub };
  }
  if (ev.kind === "system_notice") {
    const raw = ev.subtype ?? ev.text ?? "Status";
    const phaseMatch = raw.match(/Calling model: unknown \((.+)\)/);
    const toolMatch = raw.match(/Invoking tool: (.+)/);
    let label = phaseMatch ? phaseMatch[1]! : toolMatch ? `Tool: ${toolMatch[1]}` : (raw.startsWith("System: ") ? raw : `System: ${raw}`);
    const model = typeof ev.model === "string" ? ev.model : undefined;
    const text = model ? `${label} · ${model}` : label;
    return { kind: "system", text };
  }
  return { kind: ev.kind || "event", text: String(ev.kind || "event") };
}

const displayEvents = computed(() => props.block.events.map(eventDisplay));

/** Show base name for repeated blocks: "Status 2" → "Status", "toolName 2" → "toolName". */
const displayName = computed(() => {
  const n = props.block.toolName;
  const m = n.match(/^(.+) \d+$/);
  return m ? m[1]! : n;
});

function onBodyScroll() {
  const el = bodyEl.value;
  if (!el) return;
  const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < scrollThreshold;
  userScrolledUp.value = !nearBottom;
}

watch(
  () => props.block.events.length,
  async () => {
    await nextTick();
    const el = bodyEl.value;
    if (!el || userScrolledUp.value) return;
    el.scrollTop = el.scrollHeight;
  },
);
</script>

<template>
  <div class="tool-card">
    <div class="tool-card-header">
      <span class="tool-name">{{ displayName }}</span>
      <span class="tool-status">{{ block.status }}</span>
    </div>
    <div
      v-if="block.events.length"
      ref="bodyEl"
      class="tool-card-body"
      @scroll="onBodyScroll"
    >
      <div
        v-for="(disp, i) in displayEvents"
        :key="i"
        class="tool-event"
        :data-kind="disp.kind"
      >
        <template v-if="disp.toolUse">
          <div class="tool-use-name">{{ disp.toolUse.name }}</div>
          <div v-if="disp.toolUse.detail" class="tool-use-detail">{{ disp.toolUse.detail }}</div>
        </template>
        <template v-else>
          {{ disp.text }}
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.tool-card {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-raised);
  overflow: hidden;
  margin-top: 0.5rem;
}

.tool-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.5rem 0.75rem;
  font-size: 0.8125rem;
  border-bottom: 1px solid var(--border-subtle);
  background: var(--bg-subtle);
}

.tool-name {
  font-weight: 600;
  color: var(--text);
}

.tool-status {
  color: var(--text-muted);
}

.tool-card-body {
  max-height: 200px;
  overflow-y: auto;
  padding: 0.5rem 0.75rem;
}

.tool-event {
  font-size: 0.8125rem;
  color: var(--text-secondary);
  line-height: 1.4;
  padding: 0.25rem 0;
  border-bottom: 1px solid var(--border-subtle);
  white-space: pre-wrap;
  word-break: break-word;
}

.tool-event:last-child {
  border-bottom: none;
}

.tool-event[data-kind="tool_use"] {
  padding: 0.35rem 0.5rem;
  border-radius: var(--radius-sm);
  background: var(--primary-subtle);
  border: 1px solid var(--border);
}

.tool-use-name {
  font-weight: 600;
  color: var(--primary);
  font-size: 0.8125rem;
}

.tool-use-detail {
  font-size: 0.75rem;
  color: var(--text-secondary);
  margin-top: 0.2rem;
  white-space: pre-wrap;
  word-break: break-word;
}
</style>
