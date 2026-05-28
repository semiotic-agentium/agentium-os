<script setup lang="ts">
import { computed } from "vue";
import type { EventTranscriptRow } from "../../events/eventTranscriptModel";
import {
  operationalBadgeLabel,
  operationalChipClass,
  operationalLaneClass,
  operationalRailDotClass,
} from "../../events/eventTranscriptModel";
import { ingressWireBodyForDisplay, isIngressWireBody } from "../../events/ingressWireBody";
import MessageBubble from "../MessageBubble.vue";

const props = defineProps<{
  row: EventTranscriptRow;
}>();

const ingressDisplay = computed(() => {
  if (props.row.kind !== "ingress_wire") return null;
  const raw = props.row.message.text ?? "";
  if (!isIngressWireBody(raw)) return raw;
  return ingressWireBodyForDisplay(raw);
});

function showOperationalDetail(detail: string | undefined, summary: string): boolean {
  const trimmed = detail?.trim();
  if (!trimmed) return false;
  return !summary.includes(trimmed);
}
</script>

<template>
  <div
    class="event-transcript-row"
    :class="[
      `event-transcript-row--${row.kind}`,
      row.kind === 'skeleton' ? `event-transcript-row--skeleton-${row.variant}` : '',
    ]"
    :data-row-key="row.key"
  >
    <template v-if="row.kind === 'milestone'">
      <div class="event-transcript-rail" aria-hidden="true">
        <span class="event-transcript-rail-dot" :class="`event-transcript-rail-dot--${row.severity}`" />
      </div>
      <article
        class="event-lane-card event-lane-card--milestone"
        :class="`event-lane-card--${row.severity}`"
        role="status"
      >
        <header class="event-lane-card__header">
          <span class="event-lane-chip event-lane-chip--milestone">{{ row.label }}</span>
        </header>
        <div class="event-lane-card__body">
          <p class="event-lane-card__summary">{{ row.summary }}</p>
          <pre v-if="row.detail" class="event-lane-card__detail-mono">{{ row.detail }}</pre>
        </div>
      </article>
    </template>

    <template v-else-if="row.kind === 'ingress_wire'">
      <div class="event-transcript-rail" aria-hidden="true">
        <span class="event-transcript-rail-dot event-transcript-rail-dot--ingress" />
      </div>
      <article
        class="event-lane-card event-lane-card--ingress"
        :class="{ 'event-lane-card--pending': row.pending }"
        role="region"
        aria-label="Host source records ingress payload"
      >
        <header class="event-lane-card__header">
          <span class="event-lane-chip event-lane-chip--ingress">Host ingress</span>
          <span class="event-lane-card__meta" translate="no">host.source-records.v1</span>
          <span v-if="row.pending" class="event-lane-card__chip-muted">Preview</span>
        </header>
        <pre class="event-lane-card__code" translate="no">{{ ingressDisplay }}</pre>
      </article>
    </template>

    <template v-else-if="row.kind === 'operational'">
      <div class="event-transcript-rail" aria-hidden="true">
        <span class="event-transcript-rail-dot" :class="operationalRailDotClass(row.block)" />
      </div>
      <article
        class="event-lane-card"
        :class="operationalLaneClass(row.block)"
        role="note"
      >
        <header class="event-lane-card__header">
          <span class="event-lane-chip" :class="operationalChipClass(row.block)">{{
            operationalBadgeLabel(row.block)
          }}</span>
          <span v-if="row.block.agentPackage" class="event-lane-card__meta" translate="no">
            {{ row.block.agentPackage }}/{{ row.block.agentInstanceId ?? "default" }}
          </span>
        </header>
        <div class="event-lane-card__body">
          <p class="event-lane-card__summary">{{ row.block.summary }}</p>
          <p
            v-if="showOperationalDetail(row.block.detail, row.block.summary)"
            class="event-lane-card__detail"
          >
            {{ row.block.detail }}
          </p>
        </div>
      </article>
    </template>

    <template v-else-if="row.kind === 'agent_turn'">
      <div class="event-transcript-rail" aria-hidden="true">
        <span class="event-transcript-rail-dot event-transcript-rail-dot--agent" />
      </div>
      <article class="event-lane-card event-lane-card--agent" role="article">
        <header class="event-lane-card__header">
          <span class="event-lane-chip event-lane-chip--agent">Agent</span>
        </header>
        <div class="event-lane-card__body event-transcript-agent-bubble">
          <MessageBubble :message="row.message" :show-inline-streaming-dots="false" />
        </div>
      </article>
    </template>

    <template v-else-if="row.kind === 'skeleton'">
      <div class="event-transcript-rail" aria-hidden="true">
        <span class="event-transcript-rail-dot event-transcript-rail-dot--skeleton" />
      </div>
      <div
        class="event-lane-card event-lane-card--skeleton"
        :class="`event-lane-card--skeleton-${row.variant}`"
        role="status"
        aria-label="Loading transcript row"
      >
        <span class="event-transcript-skeleton-line event-transcript-skeleton-line--short" />
        <span class="event-transcript-skeleton-line" />
        <span
          v-if="row.variant === 'ingress'"
          class="event-transcript-skeleton-line event-transcript-skeleton-line--tall"
        />
      </div>
    </template>
  </div>
</template>
