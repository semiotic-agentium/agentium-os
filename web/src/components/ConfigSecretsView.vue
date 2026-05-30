<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import type { SecretOverviewEntryDto } from "../types/config";

const { fetchSecretsOverview, fetchStoreKeys, putSecret, deleteSecret } = useConfigApi();
const secrets = ref<SecretOverviewEntryDto[] | null>(null);
const storeKeys = ref<string[]>([]);
const error = ref<string | null>(null);
const refreshing = ref(false);
const linkSelection = ref<Record<string, string>>({});
const linkingKey = ref<string | null>(null);
const unlinkingKey = ref<string | null>(null);
const linkError = ref<string | null>(null);

const unsatisfiedCount = computed(() =>
  secrets.value ? secrets.value.filter((e) => !e.satisfied).length : 0,
);

const unsatisfiedSecrets = computed(() =>
  secrets.value ? secrets.value.filter((e) => !e.satisfied) : [],
);

const satisfiedSecrets = computed(() =>
  secrets.value ? secrets.value.filter((e) => e.satisfied) : [],
);

/** Store keys (from fnox/env) that can be used as link_from. Independent of linkage; M:N. */
const linkableKeyNames = computed(() => storeKeys.value);

async function loadSecrets() {
  const [overviewResult, storeKeysResult] = await Promise.all([
    fetchSecretsOverview(),
    fetchStoreKeys(),
  ]);
  if ("error" in overviewResult) {
    error.value = overviewResult.error.detail ?? overviewResult.error.title;
    return;
  }
  error.value = null;
  secrets.value = overviewResult.data;
  if ("data" in storeKeysResult) {
    storeKeys.value = storeKeysResult.data;
  } else {
    storeKeys.value = [];
  }
  syncLinkSelectionFromOverview();
}

async function refresh() {
  refreshing.value = true;
  linkError.value = null;
  await loadSecrets();
  refreshing.value = false;
}

onMounted(loadSecrets);

/** When overview loads, prefill link selection from linked_to so dropdowns show current link. */
function syncLinkSelectionFromOverview() {
  if (!secrets.value) return;
  const next: Record<string, string> = { ...linkSelection.value };
  for (const entry of secrets.value) {
    if (entry.linked_to) {
      next[entry.name] = entry.linked_to;
    }
  }
  linkSelection.value = next;
}

async function onLink(targetName: string) {
  const linkFrom = linkSelection.value[targetName]?.trim();
  if (!linkFrom) return;
  linkingKey.value = targetName;
  linkError.value = null;
  const result = await putSecret(targetName, linkFrom);
  linkingKey.value = null;
  if ("error" in result) {
    if (result.error.status === 501) {
      linkError.value =
        "Linking is not available: the runner has no runtime secret store. Start the runner with config so the UI can link secrets.";
    } else {
      linkError.value = result.error.detail ?? result.error.title;
    }
    return;
  }
  linkSelection.value = { ...linkSelection.value, [targetName]: "" };
  await loadSecrets();
}

async function onUnlink(name: string) {
  unlinkingKey.value = name;
  linkError.value = null;
  const result = await deleteSecret(name);
  unlinkingKey.value = null;
  if ("error" in result) {
    linkError.value = result.error.detail ?? result.error.title;
    return;
  }
  await loadSecrets();
}

function secretHint(name: string): string {
  return `Link "${name}" to a key from your secret store using the dropdown above (or Unlink for satisfied entries to clear the session override).`;
}
</script>

<template>
  <div class="config-secrets-view">
    <p class="config-secrets-intro">
      Link each required secret (request) to a key in your secret store. The dropdown lists keys
      that have a value in your store (fnox/env)—independent of what is currently linked. Choose a
      store key to link; Unlink clears the session override. You can re-link anytime. Satisfied =
      the runner currently has a value for that request.
    </p>

    <div class="config-secrets-toolbar">
      <button
        type="button"
        class="btn btn--secondary btn--sm"
        :disabled="refreshing || secrets === null"
        @click="refresh"
      >
        {{ refreshing ? "Refreshing…" : "Refresh" }}
      </button>
    </div>

    <p v-if="error" class="config-error" role="alert">{{ error }}</p>
    <p v-if="linkError" class="config-error" role="alert">{{ linkError }}</p>

    <template v-else-if="secrets === null">
      <div class="settings-loading">Loading secrets…</div>
    </template>

    <template v-else-if="secrets.length === 0">
      <p class="settings-empty">No secrets required by the current tool catalog or LLM config.</p>
    </template>

    <template v-else>
      <div v-if="unsatisfiedCount > 0" class="config-secrets-unsatisfied-alert" role="alert">
        <strong>{{ unsatisfiedCount }}</strong> missing — link each to a key from your secret store
        using the dropdown below.
      </div>

      <div class="config-secrets-list">
        <section
          v-if="unsatisfiedSecrets.length > 0"
          class="config-secrets-group"
          aria-labelledby="secrets-missing-heading"
        >
          <h3
            id="secrets-missing-heading"
            class="config-secrets-group-title config-secrets-group-title-missing"
          >
            Missing ({{ unsatisfiedSecrets.length }})
          </h3>
          <article
            v-for="entry in unsatisfiedSecrets"
            :key="entry.name"
            class="config-secret-card config-secret-card-unsatisfied"
          >
            <div class="config-secret-header">
              <code class="config-secret-name">{{ entry.name }}</code>
              <span
                v-if="entry.linked_to"
                class="config-secret-linked-to"
                :title="'Linked to key: ' + entry.linked_to"
              >
                Linked to <code>{{ entry.linked_to }}</code>
              </span>
              <span v-if="entry.secret_type" class="config-secret-type">{{
                entry.secret_type
              }}</span>
              <span class="config-secret-status config-secret-status-missing" aria-label="Missing">
                Missing
              </span>
            </div>
            <p v-if="entry.justification" class="config-secret-justification">
              {{ entry.justification }}
            </p>
            <p v-if="entry.descriptor" class="config-secret-descriptor">{{ entry.descriptor }}</p>

            <div v-if="linkableKeyNames.length > 0" class="config-secret-link-section">
              <label :for="'link-combo-' + entry.name" class="config-secret-link-label">Link to</label>
              <select
                :id="'link-combo-' + entry.name"
                v-model="linkSelection[entry.name]"
                class="config-secret-combo"
                :disabled="linkingKey === entry.name"
                :aria-label="'Link ' + entry.name + ' to key from store'"
              >
                <option value="">Choose a key…</option>
                <option v-for="keyName in linkableKeyNames" :key="keyName" :value="keyName">
                  {{ keyName }}
                </option>
              </select>
              <button
                type="button"
                class="btn btn--secondary btn--sm"
                :disabled="!linkSelection[entry.name] || linkingKey === entry.name"
                @click="onLink(entry.name)"
              >
                {{ linkingKey === entry.name ? "Linking…" : "Link" }}
              </button>
            </div>
            <p v-else class="config-secret-no-keys">
              No keys available to link. Add at least one secret to your store (e.g. fnox.toml or
              .env), restart the runner, then Refresh so it appears in the dropdown.
            </p>

            <div class="config-secret-consumers">
              <div v-if="entry.tool_consumers.length > 0" class="config-secret-consumer-group">
                <span class="config-secret-consumer-label">Used by tools</span>
                <ul class="config-secret-consumer-list">
                  <li v-for="tool in entry.tool_consumers" :key="tool">
                    <code>{{ tool }}</code>
                  </li>
                </ul>
              </div>
              <div v-if="entry.llm_consumers.length > 0" class="config-secret-consumer-group">
                <span class="config-secret-consumer-label">Used by LLM clients</span>
                <ul class="config-secret-consumer-list">
                  <li v-for="client in entry.llm_consumers" :key="client">
                    <code>{{ client }}</code>
                  </li>
                </ul>
              </div>
            </div>

            <p class="config-secret-hint">{{ secretHint(entry.name) }}</p>
          </article>
        </section>

        <section
          v-if="satisfiedSecrets.length > 0"
          class="config-secrets-group"
          aria-labelledby="secrets-satisfied-heading"
        >
          <h3
            id="secrets-satisfied-heading"
            class="config-secrets-group-title config-secrets-group-title-ok"
          >
            Satisfied ({{ satisfiedSecrets.length }})
          </h3>
          <article v-for="entry in satisfiedSecrets" :key="entry.name" class="config-secret-card">
            <div class="config-secret-header">
              <code class="config-secret-name">{{ entry.name }}</code>
              <span
                v-if="entry.linked_to"
                class="config-secret-linked-to"
                :title="'Linked to key: ' + entry.linked_to"
              >
                Linked to <code>{{ entry.linked_to }}</code>
              </span>
              <span v-if="entry.secret_type" class="config-secret-type">{{
                entry.secret_type
              }}</span>
              <span class="config-secret-status config-secret-status-ok" aria-label="Linked">
                Satisfied
              </span>
            </div>
            <p v-if="entry.justification" class="config-secret-justification">
              {{ entry.justification }}
            </p>
            <p v-if="entry.descriptor" class="config-secret-descriptor">{{ entry.descriptor }}</p>

            <div v-if="linkableKeyNames.length > 0" class="config-secret-link-section">
              <label :for="'link-combo-sat-' + entry.name" class="config-secret-link-label">Link to</label>
              <select
                :id="'link-combo-sat-' + entry.name"
                v-model="linkSelection[entry.name]"
                class="config-secret-combo"
                :disabled="linkingKey === entry.name"
                :aria-label="'Link ' + entry.name + ' to key from store'"
              >
                <option value="">Choose a key…</option>
                <option v-for="keyName in linkableKeyNames" :key="keyName" :value="keyName">
                  {{ keyName }}
                </option>
              </select>
              <button
                type="button"
                class="btn btn--secondary btn--sm"
                :disabled="!linkSelection[entry.name] || linkingKey === entry.name"
                @click="onLink(entry.name)"
              >
                {{ linkingKey === entry.name ? "Linking…" : "Link" }}
              </button>
            </div>
            <div class="config-secret-unlink-section">
              <button
                type="button"
                class="btn btn--ghost btn--sm"
                :disabled="unlinkingKey === entry.name"
                :aria-label="'Unlink ' + entry.name"
                @click="onUnlink(entry.name)"
              >
                {{ unlinkingKey === entry.name ? "Unlinking…" : "Unlink" }}
              </button>
            </div>

            <div class="config-secret-consumers">
              <div v-if="entry.tool_consumers.length > 0" class="config-secret-consumer-group">
                <span class="config-secret-consumer-label">Used by tools</span>
                <ul class="config-secret-consumer-list">
                  <li v-for="tool in entry.tool_consumers" :key="tool">
                    <code>{{ tool }}</code>
                  </li>
                </ul>
              </div>
              <div v-if="entry.llm_consumers.length > 0" class="config-secret-consumer-group">
                <span class="config-secret-consumer-label">Used by LLM clients</span>
                <ul class="config-secret-consumer-list">
                  <li v-for="client in entry.llm_consumers" :key="client">
                    <code>{{ client }}</code>
                  </li>
                </ul>
              </div>
            </div>

            <p class="config-secret-hint">{{ secretHint(entry.name) }}</p>
          </article>
        </section>
      </div>
    </template>
  </div>
</template>
