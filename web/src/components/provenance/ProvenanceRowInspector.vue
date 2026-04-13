<script setup lang="ts">
import { computed, ref } from "vue";
import type { ProvenanceRowBase, ProvenanceResource } from "../../types/provenance";
import {
  citationRefLabel,
  citationSimClass,
  citationSimLabel,
  driftHelp,
  driftSeverityClass,
  driftSeverityLabel,
  formatDriftScore,
  formatSelectedValue,
  payloadPriorityKeys,
  preferredSelectedRowKeys,
  rowCitationDrift,
  type SelectedRowEntry,
} from "../../utils/provenanceHelpers";

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
        v-if="
          resource === 'llm_calls' &&
          selectedRow?.drift &&
          ((selectedRow.drift as any).score != null || (selectedRow.drift as any).severity)
        "
        class="row-inspector-section"
      >
        <div class="row-inspector-section-title">
          Tactical drift
          <span class="drift-help section-title-hint" :data-tooltip="driftHelp.tactical">&#9432;</span>
        </div>
        <div class="drift-scores-grid inspector-drift-grid">
          <div class="drift-score-item">
            <span class="drift-score-label">Score</span>
            <span class="drift-score-value">{{ formatDriftScore((selectedRow.drift as any)?.score) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label">Severity</span>
            <span class="drift-score-value">{{ (selectedRow.drift as any)?.severity ?? "\u2014" }}</span>
          </div>
        </div>
      </section>

      <section
        v-if="selectedRow?.drift && (selectedRow.drift as any)?.plan"
        class="row-inspector-section"
      >
        <div class="row-inspector-section-title">
          Plan alignment
          <span class="drift-help section-title-hint" :data-tooltip="driftHelp.adherence">&#9432;</span>
        </div>
        <div class="drift-scores-grid inspector-drift-grid">
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.intent">Intent align.</span>
            <span class="drift-score-value">{{ formatDriftScore((selectedRow.drift as any)?.plan?.intentAlignment) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.step">Step align.</span>
            <span class="drift-score-value">{{ formatDriftScore((selectedRow.drift as any)?.plan?.stepAlignment) }}</span>
          </div>
          <div v-if="(selectedRow.drift as any)?.plan?.crossEncoderStepScore != null" class="drift-score-item">
            <span class="drift-score-label drift-help" data-tooltip="Cross-encoder step logit. Logit scale — always present in PlanCommitted scoring. Catches injections cosine misses.">XE step logit</span>
            <span class="drift-score-value">{{ ((selectedRow.drift as any)?.plan?.crossEncoderStepScore as number)?.toFixed(2) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.trajectory">Trajectory</span>
            <span class="drift-score-value">{{ formatDriftScore((selectedRow.drift as any)?.plan?.trajectoryDrift) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.adherence">Adherence</span>
            <span class="drift-score-value">{{ formatDriftScore((selectedRow.drift as any)?.plan?.planAdherenceScore) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.composite">Composite</span>
            <span :class="['planning-step-pill', driftSeverityClass((selectedRow.drift as any)?.plan?.compositeSeverity)]">
              {{ driftSeverityLabel((selectedRow.drift as any)?.plan?.compositeSeverity) }}
            </span>
          </div>
        </div>
        <div v-if="(selectedRow.drift as any)?.intentTextPreview" class="drift-preview-pair">
          <div class="drift-preview">
            <div class="drift-preview-label">Intent</div>
            <div class="drift-preview-text">{{ (selectedRow.drift as any).intentTextPreview }}</div>
          </div>
          <div class="drift-preview">
            <div class="drift-preview-label">Response</div>
            <div class="drift-preview-text">{{ (selectedRow.drift as any).responseTextPreview }}</div>
          </div>
        </div>
      </section>

      <section
        v-if="
          resource === 'llm_calls' &&
          selectedRow &&
          (rowCitationDrift(selectedRow) || selectedRow.drift)
        "
        class="row-inspector-section"
      >
        <template v-if="rowCitationDrift(selectedRow)">
          <div class="row-inspector-section-title">
            Grounding
            <span class="cite-mean-badge" :class="citationSimClass(rowCitationDrift(selectedRow)!.meanSimilarity, false)">
              mean {{ rowCitationDrift(selectedRow)!.meanSimilarity.toFixed(2) }}
            </span>
            <span class="drift-help section-title-hint" :data-tooltip="driftHelp.grounding">&#9432;</span>
          </div>
          <p class="inspector-grounding-lede">
            Embedding similarity between the response and each cited snippet. Not a factual truth check.
            <span class="drift-help drift-inline-help" :data-tooltip="driftHelp.grounding">&#9432;</span>
          </p>
          <div class="cite-list">
            <div
              v-for="(c, i) in rowCitationDrift(selectedRow)!.perCitation"
              :key="`cite-${i}`"
              class="cite-entry"
              :class="{ 'cite-entry-negated': c.negated }"
            >
              <div class="cite-header">
                <span class="cite-ref-tag" :class="c.isHistory ? 'cite-ref-history' : 'cite-ref-archive'">
                  {{ citationRefLabel(c) }}
                </span>
                <span v-if="c.negated" class="cite-negated-badge">counter-evidence</span>
                <span class="cite-sim-pill" :class="citationSimClass(c.similarity, c.negated)">
                  {{ c.similarity.toFixed(2) }} &middot; {{ citationSimLabel(c.similarity, c.negated) }}
                </span>
                <span v-if="c.isHistory" class="cite-kind-label">history</span>
                <span v-else class="cite-kind-label">archive</span>
              </div>
              <pre v-if="c.contentPreview" class="cite-content">{{ c.contentPreview }}</pre>
              <div v-else class="cite-content-empty">content not resolved</div>
            </div>
          </div>
          <details class="cite-threshold-legend">
            <summary>Threshold legend</summary>
            <ul class="cite-threshold-list">
              <li><strong>Strong (&ge;0.65):</strong> answer closely paraphrases the cited snippet.</li>
              <li><strong>Moderate (0.40&ndash;0.65):</strong> same domain, partial overlap.</li>
              <li><strong>Weak (&lt;0.40):</strong> likely wrong ref or weak tie to cited text.</li>
              <li><strong>Counter:</strong> model flagged contradicting evidence; excluded from mean.</li>
            </ul>
          </details>
        </template>
        <template v-else-if="selectedRow.drift">
          <div class="row-inspector-section-title">Grounding</div>
          <p class="inspector-grounding-empty">{{ driftHelp.groundingEmpty }}</p>
        </template>
      </section>
    </div>
  </section>
</template>
