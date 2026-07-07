<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import type { GateAuthorizationSummary } from "../chat/gateAuthorizationUi";
import { navigateToTrustSettings } from "../events/operatorRoute";

defineProps<{
  summary: GateAuthorizationSummary;
  agentPackage?: string | null;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  approve: [];
  deny: [];
}>();
</script>

<template>
  <section
    class="gate-auth-card"
    role="region"
    aria-label="Tier-3 gate authorization"
    data-testid="gate-authorization-card"
  >
    <header class="gate-auth-card__header">
      <span class="gate-auth-card__badge">Tier {{ summary.tier }} authorization</span>
      <span class="gate-auth-card__label">Human approval required</span>
    </header>
    <p class="gate-auth-card__intent">
      <span class="gate-auth-card__field-label">Grounded intent</span>
      {{ summary.groundedIntent }}
    </p>
    <p v-if="summary.postconditionCount > 0" class="gate-auth-card__postconditions">
      <span class="gate-auth-card__field-label">Postconditions</span>
      {{ summary.postconditionCount }} declared verification check{{
        summary.postconditionCount === 1 ? "" : "s"
      }}
    </p>
    <p class="gate-auth-card__hint">
      Approve to execute the grounded tier-{{ summary.tier }} action. Deny to cancel without
      running the tool.
    </p>
    <div class="gate-auth-card__actions">
      <button
        type="button"
        class="gate-auth-card__btn gate-auth-card__btn--approve"
        data-testid="gate-authorization-approve"
        :disabled="disabled"
        @click="emit('approve')"
      >
        Approve
      </button>
      <button
        type="button"
        class="gate-auth-card__btn gate-auth-card__btn--deny"
        data-testid="gate-authorization-deny"
        :disabled="disabled"
        @click="emit('deny')"
      >
        Deny
      </button>
      <button
        v-if="agentPackage"
        type="button"
        class="gate-auth-card__btn gate-auth-card__btn--link"
        data-testid="gate-authorization-trust-policy"
        :disabled="disabled"
        @click="navigateToTrustSettings(agentPackage)"
      >
        View trust policy
      </button>
    </div>
  </section>
</template>
