<script setup lang="ts">
import { computed, ref } from "vue";
import type { ChatMessage } from "../types/a2a";
import {
  parseCoordinatorAnswer,
  safeHostname,
  isUrl,
  type ParsedCoordinatorAnswer,
} from "../utils/parseCoordinatorAnswer";

const props = defineProps<{ messages: ChatMessage[] }>();

const latestInterpretation = computed((): ParsedCoordinatorAnswer | null => {
  for (let i = props.messages.length - 1; i >= 0; i--) {
    const msg = props.messages[i]!;
    if (msg.role !== "agent" || !msg.text) continue;
    const parsed = parseCoordinatorAnswer(msg.text);
    if (parsed) return parsed;
  }
  return null;
});

const COLLAPSE_THRESHOLD = 3;

const showAllGoals = ref(false);
const showAllGaps = ref(false);
const showAllDecisions = ref(false);
const showAllRisks = ref(false);
const showAllFollowUps = ref(false);

function visibleItems<T>(items: T[], showAll: boolean): T[] {
  return showAll ? items : items.slice(0, COLLAPSE_THRESHOLD);
}

function truncate(text: string, max: number): string {
  return text.length > max ? text.slice(0, max) + "…" : text;
}
</script>

<template>
  <div class="dashboard-card">
    <div class="dashboard-card-header">
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
        <line x1="16" y1="13" x2="8" y2="13" />
        <line x1="16" y1="17" x2="8" y2="17" />
        <polyline points="10 9 9 9 8 9" />
      </svg>
      Latest Interpretation
    </div>
    <div class="dashboard-card-body">
      <template v-if="latestInterpretation">
        <!-- Answer preview -->
        <div class="interpretation-section">
          <div class="interpretation-answer-preview">
            {{ truncate(latestInterpretation.answer, 150) }}
          </div>
        </div>

        <!-- Goals -->
        <div v-if="latestInterpretation.actionableGoals.length > 0" class="interpretation-section">
          <div class="interpretation-section-label">Goals</div>
          <ol class="interpretation-list">
            <li
              v-for="(g, i) in visibleItems(latestInterpretation.actionableGoals, showAllGoals)"
              :key="i"
            >
              {{ g.goal }}
              <span v-if="g.owner" class="goal-meta">Owner: {{ g.owner }}</span>
              <span v-if="g.dueDate" class="goal-meta">Due: {{ g.dueDate }}</span>
            </li>
          </ol>
          <button
            v-if="latestInterpretation.actionableGoals.length > COLLAPSE_THRESHOLD"
            class="interpretation-toggle"
            @click="showAllGoals = !showAllGoals"
          >
            {{
              showAllGoals
                ? "Show less"
                : `Show ${latestInterpretation.actionableGoals.length - COLLAPSE_THRESHOLD} more`
            }}
          </button>
        </div>

        <!-- Gaps -->
        <div v-if="latestInterpretation.gaps.length > 0" class="interpretation-section">
          <div class="interpretation-section-label interpretation-label-warning">Gaps</div>
          <ul class="interpretation-list">
            <li v-for="(g, i) in visibleItems(latestInterpretation.gaps, showAllGaps)" :key="i">
              {{ g }}
            </li>
          </ul>
          <button
            v-if="latestInterpretation.gaps.length > COLLAPSE_THRESHOLD"
            class="interpretation-toggle"
            @click="showAllGaps = !showAllGaps"
          >
            {{
              showAllGaps
                ? "Show less"
                : `Show ${latestInterpretation.gaps.length - COLLAPSE_THRESHOLD} more`
            }}
          </button>
        </div>

        <!-- Decisions -->
        <div v-if="latestInterpretation.decisions.length > 0" class="interpretation-section">
          <div class="interpretation-section-label">Decisions</div>
          <ul class="interpretation-list">
            <li
              v-for="(d, i) in visibleItems(latestInterpretation.decisions, showAllDecisions)"
              :key="i"
            >
              {{ d }}
            </li>
          </ul>
          <button
            v-if="latestInterpretation.decisions.length > COLLAPSE_THRESHOLD"
            class="interpretation-toggle"
            @click="showAllDecisions = !showAllDecisions"
          >
            {{
              showAllDecisions
                ? "Show less"
                : `Show ${latestInterpretation.decisions.length - COLLAPSE_THRESHOLD} more`
            }}
          </button>
        </div>

        <!-- Risks -->
        <div v-if="latestInterpretation.risks.length > 0" class="interpretation-section">
          <div class="interpretation-section-label interpretation-label-danger">Risks</div>
          <ul class="interpretation-list interpretation-list-danger">
            <li v-for="(r, i) in visibleItems(latestInterpretation.risks, showAllRisks)" :key="i">
              {{ r }}
            </li>
          </ul>
          <button
            v-if="latestInterpretation.risks.length > COLLAPSE_THRESHOLD"
            class="interpretation-toggle"
            @click="showAllRisks = !showAllRisks"
          >
            {{
              showAllRisks
                ? "Show less"
                : `Show ${latestInterpretation.risks.length - COLLAPSE_THRESHOLD} more`
            }}
          </button>
        </div>

        <!-- Follow-ups -->
        <div v-if="latestInterpretation.followUps.length > 0" class="interpretation-section">
          <div class="interpretation-section-label">Follow-ups</div>
          <ul class="interpretation-list">
            <li
              v-for="(f, i) in visibleItems(latestInterpretation.followUps, showAllFollowUps)"
              :key="i"
            >
              {{ f }}
            </li>
          </ul>
          <button
            v-if="latestInterpretation.followUps.length > COLLAPSE_THRESHOLD"
            class="interpretation-toggle"
            @click="showAllFollowUps = !showAllFollowUps"
          >
            {{
              showAllFollowUps
                ? "Show less"
                : `Show ${latestInterpretation.followUps.length - COLLAPSE_THRESHOLD} more`
            }}
          </button>
        </div>

        <!-- Sources -->
        <div v-if="latestInterpretation.sources.length > 0" class="interpretation-section">
          <div class="interpretation-section-label">Sources</div>
          <div class="source-chips">
            <template v-for="src in latestInterpretation.sources" :key="src">
              <a
                v-if="isUrl(src)"
                :href="src"
                target="_blank"
                rel="noopener noreferrer"
                class="source-chip"
              >
                {{ safeHostname(src) }}
              </a>
              <span v-else class="source-chip">{{ src }}</span>
            </template>
          </div>
        </div>
      </template>

      <!-- Empty state -->
      <div v-else class="empty-state">
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
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </svg>
        <span class="empty-state-text">No coordinator analysis yet</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.interpretation-section {
  margin-bottom: 12px;
}

.interpretation-section:last-child {
  margin-bottom: 0;
}

.interpretation-section-label {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--text-muted);
  font-weight: 600;
  margin-bottom: 4px;
}

.interpretation-label-warning {
  color: var(--color-warning);
}

.interpretation-label-danger {
  color: var(--color-error);
}

.interpretation-answer-preview {
  font-size: 12px;
  color: var(--text-secondary);
  line-height: 1.5;
  display: -webkit-box;
  -webkit-line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.interpretation-list {
  margin: 0;
  padding-left: 16px;
  font-size: 12px;
  color: var(--text-secondary);
  line-height: 1.5;
}

.interpretation-list li {
  margin-bottom: 2px;
}

.interpretation-list-danger li {
  color: var(--color-error);
}

.interpretation-toggle {
  background: none;
  border: none;
  padding: 0;
  margin-top: 4px;
  font-size: 11px;
  color: var(--primary);
  cursor: pointer;
}

.interpretation-toggle:hover {
  text-decoration: underline;
}

.interpretation-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 24px 16px;
}
</style>
