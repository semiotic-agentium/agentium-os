<script setup lang="ts">
import type { ContextPlanningTaskSnapshot } from "../../types/provenance";
import { computed } from "vue";
import {
  driftHelp,
  driftSeverityClass,
  driftSeverityLabel,
  formatDriftScore,
  groundingCallCount,
  meanPositiveSimilarity,
  planningIntentLabel,
  planningTaskTitle,
  taskHasDrift,
  citationSimClass,
} from "../../utils/provenanceHelpers";

const props = defineProps<{
  planningTasks: ContextPlanningTaskSnapshot[];
}>();

const driftTasks = computed(() => props.planningTasks.filter(taskHasDrift));

const emit = defineEmits<{
  (e: "drillToDriftCalls", taskId: string): void;
}>();
</script>

<template>
  <div class="provenance-section-title drift-section-title">
    Plan drift
    <span class="drift-help drift-tab-help" :data-tooltip="driftHelp.planDriftTab">&#9432;</span>
  </div>
  <p class="drift-tab-lede">
    Task-level <strong>plan alignment</strong> below; each call can include <strong>grounding</strong> (answer vs cited snippet — not factual verification).
  </p>
  <div v-if="planningTasks.length === 0" class="provenance-empty">
    No planning data available. Drift analysis requires committed intents and plans.
  </div>
  <div v-else-if="driftTasks.length === 0" class="provenance-empty">
    No drift data collected yet. Drift scores appear after LLM calls within a committed plan.
  </div>
  <div v-else class="drift-task-list">
    <article
      v-for="task in driftTasks"
      :key="`drift:${task.taskId}`"
      class="drift-task-card"
    >
      <!-- Header: task name + composite severity -->
      <div class="drift-task-header">
        <span class="group-key">{{ planningTaskTitle(task) }}</span>
        <button
          :class="['planning-step-pill', 'drift-count-link', driftSeverityClass(task.drift?.compositeSeverity)]"
          @click="emit('drillToDriftCalls', task.taskId)"
          title="View all LLM calls for this task in Explore"
        >{{ driftSeverityLabel(task.drift?.compositeSeverity) }}</button>
      </div>
      <div class="drift-task-intent">{{ planningIntentLabel(task) }}</div>

      <!-- Summary: single adherence gauge + call counts -->
      <div class="drift-call-summary">
        <span>{{ task.drift?.scoredCallCount ?? 0 }} calls scored</span>
        <button
          v-if="(task.drift?.warnCount ?? 0) > 0"
          class="drift-count-link drift-warn-count"
          @click="emit('drillToDriftCalls', task.taskId)"
          title="View LLM calls for this task"
        >{{ task.drift?.warnCount }} warn</button>
        <button
          v-if="(task.drift?.blockCount ?? 0) > 0"
          class="drift-count-link drift-block-count"
          @click="emit('drillToDriftCalls', task.taskId)"
          title="View LLM calls for this task"
        >{{ task.drift?.blockCount }} block</button>
      </div>

      <div v-if="groundingCallCount(task) > 0" class="drift-grounding-summary">
        <strong>Grounding:</strong> {{ groundingCallCount(task) }} call(s) with resolved citations
      </div>

      <div class="drift-bar-row">
        <span class="drift-bar-label drift-help" :data-tooltip="driftHelp.adherence">Adherence</span>
        <div class="drift-bar-track">
          <div
            class="drift-bar-fill"
            :class="driftSeverityClass(task.drift?.compositeSeverity)"
            :style="{ transform: `scaleX(${task.drift?.planAdherenceScore ?? 0})` }"
          />
        </div>
        <span class="drift-bar-value">{{ formatDriftScore(task.drift?.planAdherenceScore) }}</span>
      </div>

      <!-- Evidence: inline cards for each warn/block call -->
      <div
        v-if="task.drift?.driftedCalls && task.drift.driftedCalls.length > 0"
        class="drift-evidence-list"
      >
        <div
          v-for="(call, ci) in task.drift.driftedCalls"
          :key="`evidence:${task.taskId}:${call.functionName}:${call.severity}:${ci}`"
          class="drift-evidence"
        >
          <div class="drift-evidence-header">
            <span :class="['planning-step-pill', driftSeverityClass(call.severity)]">
              {{ call.severity }}
            </span>
            <span class="drift-evidence-fn">{{ call.functionName }}</span>
          </div>
          <div class="drift-preview-pair">
            <div class="drift-preview">
              <div class="drift-preview-label">{{ call.stepTextPreview ? 'Step' : 'Intent' }}</div>
              <div class="drift-preview-text">{{ call.stepTextPreview || call.intentTextPreview || '—' }}</div>
            </div>
            <div class="drift-preview">
              <div class="drift-preview-label">Response</div>
              <div class="drift-preview-text">{{ call.responseTextPreview || '—' }}</div>
            </div>
          </div>
          <div class="drift-evidence-scores">
            <span v-if="call.stepAlignment != null">step={{ call.stepAlignment.toFixed(2) }}</span>
            <span>intent={{ call.intentAlignment.toFixed(2) }}</span>
            <span v-if="call.crossEncoderStepScore != null">XE={{ call.crossEncoderStepScore.toFixed(1) }}</span>
            <button class="drift-count-link" @click="emit('drillToDriftCalls', task.taskId)">View in Explore</button>
          </div>

          <div
            v-if="call.citations && call.citations.length > 0"
            class="drift-grounding-compact"
          >
            {{ call.citations.length }} citation(s) · mean {{ meanPositiveSimilarity(call.citations).toFixed(2) }}
            <span class="drift-help drift-inline-help" :data-tooltip="driftHelp.grounding">&#9432;</span>
          </div>

          <!-- Inline citation evidence for this call -->
          <div v-if="call.citations && call.citations.length > 0" class="drift-cite-section">
            <div class="drift-cite-header">
              Grounding — cited snippets
              <span class="drift-help drift-inline-help" :data-tooltip="driftHelp.grounding">&#9432;</span>
            </div>
            <p class="drift-cite-explainer">
              Similarity measures how much the response resembles each cited entry (not whether facts are true). Calibrated bands: &ge;0.65 strong, 0.40&ndash;0.65 moderate, &lt;0.40 weak. Full reference: <code>docs/drift-catalogue.md</code> in the repo.
            </p>
            <details class="cite-threshold-legend">
              <summary>Threshold legend</summary>
              <ul class="cite-threshold-list">
                <li><strong>Strong (&ge;0.65):</strong> answer closely paraphrases the cited snippet.</li>
                <li><strong>Moderate (0.40&ndash;0.65):</strong> same domain, partial overlap.</li>
                <li><strong>Weak (&lt;0.40):</strong> likely wrong ref or weak tie to cited text.</li>
                <li><strong>Counter:</strong> model flagged contradicting evidence; excluded from mean.</li>
              </ul>
            </details>
            <div class="cite-list cite-list-compact">
              <div
                v-for="(c, ci) in call.citations"
                :key="`dcite-${ci}`"
                class="cite-entry"
                :class="{ 'cite-entry-negated': c.negated }"
              >
                <div class="cite-header">
                  <span class="cite-ref-tag" :class="c.isHistory ? 'cite-ref-history' : 'cite-ref-archive'">
                    {{ c.raw }}
                  </span>
                  <span v-if="c.negated" class="cite-negated-badge">counter</span>
                  <span class="cite-sim-pill" :class="citationSimClass(c.similarity, c.negated)">
                    {{ c.similarity.toFixed(2) }}
                  </span>
                </div>
                <div class="cite-content cite-content-compact">{{ c.contentPreview || '—' }}</div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- No scored calls -->
      <div
        v-else-if="(task.drift?.scoredCallCount ?? 0) === 0"
        class="drift-clean-msg"
      >
        No calls scored yet.
      </div>

      <!-- Dimensions: collapsible, default closed -->
      <details class="drift-dimensions-toggle">
        <summary class="drift-dimensions-summary">Dimensions</summary>
        <div class="drift-scores-grid">
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.intent">Intent</span>
            <span class="drift-score-value">{{ formatDriftScore(task.drift?.intentAlignment) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.step">Step</span>
            <span class="drift-score-value">{{ formatDriftScore(task.drift?.stepAlignment) }}</span>
          </div>
          <div class="drift-score-item" v-if="task.drift?.crossEncoderStepScore != null">
            <span class="drift-score-label drift-help" data-tooltip="Cross-encoder (JINA) logit for step vs response. Higher = more relevant. Combined with cosine.">XE</span>
            <span class="drift-score-value">{{ (task.drift.crossEncoderStepScore as number).toFixed(2) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.trajectory">Trajectory</span>
            <span class="drift-score-value">{{ formatDriftScore(task.drift?.trajectoryDrift) }}</span>
          </div>
          <div class="drift-score-item">
            <span class="drift-score-label drift-help" :data-tooltip="driftHelp.adherence">Adherence</span>
            <span class="drift-score-value">{{ formatDriftScore(task.drift?.planAdherenceScore) }}</span>
          </div>
        </div>
        <div class="drift-dimension-bars">
          <div class="drift-bar-row">
            <span class="drift-bar-label">Intent</span>
            <div class="drift-bar-track"><div class="drift-bar-fill drift-severity-ok" :style="{ transform: `scaleX(${task.drift?.intentAlignment ?? 0})` }" /></div>
          </div>
          <div class="drift-bar-row">
            <span class="drift-bar-label">Step</span>
            <div class="drift-bar-track"><div class="drift-bar-fill drift-severity-ok" :style="{ transform: `scaleX(${task.drift?.stepAlignment ?? 0})` }" /></div>
          </div>
          <div class="drift-bar-row">
            <span class="drift-bar-label">Trajectory</span>
            <div class="drift-bar-track"><div class="drift-bar-fill drift-severity-ok" :style="{ transform: `scaleX(${task.drift?.trajectoryDrift ?? 0})` }" /></div>
          </div>
          <div class="drift-bar-row">
            <span class="drift-bar-label">Adherence</span>
            <div class="drift-bar-track"><div class="drift-bar-fill" :class="driftSeverityClass(task.drift?.compositeSeverity)" :style="{ transform: `scaleX(${task.drift?.planAdherenceScore ?? 0})` }" /></div>
          </div>
        </div>
      </details>
    </article>
  </div>
</template>
