<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import type { SemioticPolicy } from "../types/config";

defineProps<{
  policy: SemioticPolicy;
  idPrefix?: string;
}>();
</script>

<template>
  <div class="semiotic-policy-fields">
    <fieldset class="semiotic-policy-fieldset">
      <legend class="semiotic-policy-legend">Activation</legend>
      <label class="config-field config-checkbox">
        <input v-model="policy.enabled" type="checkbox" />
        <span>Enable semiotic gate</span>
      </label>
      <label class="config-field">
        <span class="config-label">Mode</span>
        <select v-model="policy.mode" class="config-input config-select">
          <option value="dry_run">Dry run (audit only)</option>
          <option value="enforce">Enforce (block ungrounded calls)</option>
        </select>
        <span class="config-hint semiotic-field-hint">
          Dry run records gate decisions without blocking tool execution.
        </span>
      </label>
    </fieldset>

    <fieldset class="semiotic-policy-fieldset">
      <legend class="semiotic-policy-legend">Threshold</legend>
      <label class="config-field">
        <span class="config-label">Enforce min tier</span>
        <input
          v-model.number="policy.enforceMinTier"
          class="config-input"
          type="number"
          min="0"
          max="3"
        />
        <span class="config-hint semiotic-field-hint">
          0 read · 1 telemetry · 2 write · 3 delete / human auth
        </span>
      </label>
    </fieldset>

    <fieldset class="semiotic-policy-fieldset">
      <legend class="semiotic-policy-legend">Tier 3</legend>
      <label class="config-field config-checkbox">
        <input v-model="policy.requirePostconditionsT3" type="checkbox" />
        <span>Require postconditions at tier 3</span>
      </label>
      <label class="config-field config-checkbox">
        <input v-model="policy.strictCitationAnchors" type="checkbox" />
        <span>Strict citation anchors</span>
        <span class="config-hint semiotic-field-hint">
          #N / @N references must resolve in the provenance graph.
        </span>
      </label>
    </fieldset>
  </div>
</template>
