<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, ref } from "vue";
import type { ProvenanceRowBase, ProvenanceResource } from "../../types/provenance";
import {
  citationRefLabel,
  formatSelectedValue,
  payloadPriorityKeys,
  preferredSelectedRowKeys,
  type SelectedRowEntry,
} from "../../utils/provenanceHelpers";
import {
  gateDecisionClass,
  gateDecisionLabel,
  gateHelp,
  gateReasonLabel,
  integrityStatusClass,
  rowCitationIntegrity,
  rowGateDecision,
} from "../../utils/gateHelpers";

const props = defineProps<{
  selectedRow: ProvenanceRowBase;
  resource: ProvenanceResource;
}>();

const emit = defineEmits<{
  (e: "close"): void;
}>();

const inspectorCollapsed = ref(false);

const selectedRowEntries = computed<SelectedRowEntry[]>(() => {
  const row = props.selectedRow;
  if (!row) return [];
  const keys = Object.keys(row);
  const orderedKeys = [
    ...preferredSelectedRowKeys.filter((key) => keys.includes(key)),
    ...keys.filter((key) => !preferredSelectedRowKeys.includes(key)),
  ];
  return orderedKeys.map((key) => {
    const formatted = formatSelectedValue(row[key], key);
    return { key, kind: formatted.kind, display: formatted.display };
  });
});

const payloadEntries = computed(() =>
  selectedRowEntries.value.filter((entry) => payloadPriorityKeys.has(entry.key)),
);

const detailEntries = computed(() =>
  selectedRowEntries.value.filter((entry) => !payloadPriorityKeys.has(entry.key)),
);

const gateRow = computed(() => rowGateDecision(props.selectedRow));
const gateDeficientNodes = computed((): string[] => {
  const row = gateRow.value;
  if (!row) return [];
  const raw = row.deficientNodes ?? row.deficient_nodes;
  return Array.isArray(raw) ? raw.map(String) : [];
});
const citationIntegrity = computed(() => rowCitationIntegrity(props.selectedRow));

const integrityEntries = computed(() => {
  const cit = citationIntegrity.value;
  if (!cit) return [];
  const per = cit.perCitation ?? cit.per_citation;
  if (Array.isArray(per)) return per as Array<Record<string, unknown>>;
  return [];
});
</script>

<template>
  <section class="row-inspector">
    <div class="row-inspector-title">
      <span>Selected activity details</span>
      <div class="row-inspector-actions">
        <button class="action-btn small" @click="inspectorCollapsed = !inspectorCollapsed">
          {{ inspectorCollapsed ? "Expand" : "Collapse" }}
        </button>
        <button
          class="action-btn small"
          @click="emit('close')"
        >
          Close
        </button>
      </div>
    </div>

    <div v-if="!inspectorCollapsed" class="row-inspector-body">
      <section v-if="payloadEntries.length > 0" class="row-inspector-section">
        <div class="row-inspector-section-title">Call/Result payloads</div>
        <div
          v-for="entry in payloadEntries"
          :key="`payload:${entry.key}`"
          class="row-inspector-item payload"
        >
          <div class="row-inspector-key">{{ entry.key }}</div>
          <pre v-if="entry.kind === 'json'" class="row-inspector-json">{{ entry.display }}</pre>
          <div v-else class="row-inspector-value">{{ entry.display }}</div>
        </div>
      </section>

      <section class="row-inspector-section">
        <div class="row-inspector-section-title">Activity fields</div>
        <div
          v-for="entry in detailEntries"
          :key="`detail:${entry.key}`"
          class="row-inspector-item"
        >
          <div class="row-inspector-key">{{ entry.key }}</div>
          <pre v-if="entry.kind === 'json'" class="row-inspector-json">{{ entry.display }}</pre>
          <div v-else class="row-inspector-value">{{ entry.display }}</div>
        </div>
      </section>

      <section
        v-if="resource === 'tool_calls' && gateRow"
        class="row-inspector-section"
      >
        <div class="row-inspector-section-title">
          Gate decision
          <span class="gate-help section-title-hint" :data-tooltip="gateHelp.gateTab">&#9432;</span>
        </div>
        <div class="gate-scores-grid inspector-gate-grid">
          <div class="gate-score-item">
            <span class="gate-score-label">Tier</span>
            <span class="gate-score-value">{{ gateRow.tier ?? "—" }}</span>
          </div>
          <div class="gate-score-item">
            <span class="gate-score-label">Decision</span>
            <span :class="['planning-step-pill', gateDecisionClass(String(gateRow.decision ?? ''))]">
              {{ gateDecisionLabel(String(gateRow.decision ?? '')) }}
            </span>
          </div>
          <div class="gate-score-item">
            <span class="gate-score-label">Reason</span>
            <span class="gate-score-value">{{ gateReasonLabel(String(gateRow.reasonCode ?? gateRow.reason_code ?? '')) }}</span>
          </div>
        </div>
        <div v-if="gateDeficientNodes.length > 0" class="gate-deficit-list">
          <span v-for="node in gateDeficientNodes" :key="node" class="gate-deficit-chip">{{ node }}</span>
        </div>
        <p v-else class="inspector-gate-empty">No deficient nodes recorded.</p>
      </section>

      <section
        v-if="resource === 'llm_calls' && integrityEntries.length > 0"
        class="row-inspector-section"
      >
        <div class="row-inspector-section-title">
          Citation integrity
          <span class="gate-help section-title-hint" :data-tooltip="gateHelp.citationIntegrity">&#9432;</span>
        </div>
        <p class="inspector-integrity-lede">
          Whether each citation ref resolved in the provenance graph — not factual verification.
        </p>
        <div class="integrity-list">
          <div
            v-for="(c, i) in integrityEntries"
            :key="`integrity-${i}`"
            class="integrity-entry"
          >
            <div class="integrity-header">
              <span class="cite-ref-tag" :class="c.isHistory ? 'cite-ref-history' : 'cite-ref-archive'">
                {{ citationRefLabel(c as any) }}
              </span>
              <span
                class="integrity-status-chip"
                :class="integrityStatusClass(String(c.status ?? 'unknown'))"
              >
                {{ c.status ?? "unknown" }}
              </span>
              <span v-if="c.negated" class="cite-negated-badge">counter-evidence</span>
            </div>
            <pre v-if="c.contentPreview ?? c.content_preview" class="cite-content">{{ c.contentPreview ?? c.content_preview }}</pre>
          </div>
        </div>
      </section>
    </div>
  </section>
</template>
