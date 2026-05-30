<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { ref, watch, nextTick, computed } from "vue";
import type { ToolNotificationBlock } from "../types/a2a";
import {
  buildDisplayEvents,
  buildFsmSteps,
  parseToolNameParts,
} from "../chat/toolCardDisplay";
import ToolCardFsmRail from "./tool-card/ToolCardFsmRail.vue";
import ToolCardHeaderBar from "./tool-card/ToolCardHeaderBar.vue";
import ToolCardLabeledPre from "./tool-card/ToolCardLabeledPre.vue";
import ToolCardSystemRow from "./tool-card/ToolCardSystemRow.vue";
import ToolCardToolUseBlock from "./tool-card/ToolCardToolUseBlock.vue";

const props = defineProps<{ block: ToolNotificationBlock }>();

const bodyEl = ref<HTMLElement | null>(null);
const userScrolledUp = ref(false);
const scrollThreshold = 60;

const displayEvents = computed(() => buildDisplayEvents(props.block.events));

const fsmSteps = computed(() => buildFsmSteps(props.block.events, props.block.status));

const nameParts = computed(() => parseToolNameParts(props.block.toolName));

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
    <ToolCardHeaderBar
      :display-name="nameParts.displayName"
      :status="block.status"
      :ordinal="nameParts.ordinal"
      :has-explicit-ordinal="nameParts.hasExplicitOrdinal"
    />
    <ToolCardFsmRail v-if="fsmSteps.length > 0" :steps="fsmSteps" />
    <details v-if="displayEvents.length" class="tool-card-details">
      <summary class="tool-card-details-summary">
        Show {{ displayEvents.length }} event{{ displayEvents.length === 1 ? "" : "s" }}
      </summary>
      <div ref="bodyEl" class="tool-card-body" @scroll="onBodyScroll">
        <div v-for="(disp, i) in displayEvents" :key="i" class="tool-event" :data-kind="disp.kind">
          <ToolCardToolUseBlock
            v-if="disp.toolUse"
            :name="disp.toolUse.name"
            :detail="disp.toolUse.detail || undefined"
          />
          <ToolCardSystemRow
            v-else-if="disp.kind === 'system'"
            :text="disp.text"
            :count="disp.count"
            :active="i === displayEvents.length - 1 && block.status === 'Running'"
          />
          <ToolCardLabeledPre
            v-else-if="disp.kind === 'read'"
            label="Read"
            :text="disp.text"
            variant="read"
          />
          <ToolCardLabeledPre
            v-else-if="disp.kind === 'step_detail'"
            label="Step"
            :text="disp.text"
            variant="step"
          />
          <ToolCardLabeledPre
            v-else-if="disp.kind === 'comms_outbound'"
            label="Outbound message"
            :text="disp.text"
            variant="comms"
          />
          <ToolCardLabeledPre
            v-else-if="disp.kind === 'failure'"
            label="Execution error"
            :text="disp.text"
            variant="failure"
          />
          <template v-else>
            {{ disp.text }}
          </template>
        </div>
      </div>
    </details>
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

.tool-card-details {
  border-top: 1px solid var(--border-subtle);
}

.tool-card-details-summary {
  cursor: pointer;
  list-style: none;
  user-select: none;
  padding: 0.4rem 0.75rem;
  font-size: 0.75rem;
  color: var(--text-muted);
  background: var(--bg-subtle);
  transition: color 0.15s ease, background-color 0.15s ease;
}

.tool-card-details-summary:hover {
  color: var(--text-secondary);
}

.tool-card-details-summary::-webkit-details-marker {
  display: none;
}

.tool-card-details-summary::before {
  content: "▸ ";
  display: inline-block;
}

.tool-card-details[open] > .tool-card-details-summary::before {
  content: "▾ ";
}

.tool-card-body {
  max-height: 200px;
  overflow-y: auto;
  padding: 0.5rem 0.75rem;
  border-top: 1px solid var(--border-subtle);
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

.tool-event[data-kind="tool_use"],
.tool-event[data-kind="read"],
.tool-event[data-kind="step_detail"],
.tool-event[data-kind="comms_outbound"],
.tool-event[data-kind="failure"] {
  padding: 0.35rem 0;
}
</style>
