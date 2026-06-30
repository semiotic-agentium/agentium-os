<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { useInstanceClient } from "../composables/useInstanceClient";

const emit = defineEmits<{
  connected: [];
}>();

const { draftUrl, draftToken, connectionError, status, connect } = useInstanceClient();

async function onSubmit(): Promise<void> {
  const ok = await connect();
  if (ok) emit("connected");
}
</script>

<template>
  <div class="instance-connect">
    <div class="instance-connect__card panel">
      <h1 class="instance-connect__title">Connect to Agentium OS</h1>
      <p class="instance-connect__lede">
        This console talks to a running server instance. Enter its URL and operator token (if
        required).
      </p>

      <form class="instance-connect__form" @submit.prevent="onSubmit">
        <label class="instance-connect__field">
          <span class="instance-connect__label">Instance URL</span>
          <input
            v-model="draftUrl"
            class="input"
            type="url"
            name="instance-url"
            placeholder="http://127.0.0.1:18080"
            autocomplete="url"
            required
          />
        </label>
        <label class="instance-connect__field">
          <span class="instance-connect__label">Runner token (optional)</span>
          <input
            v-model="draftToken"
            class="input"
            type="password"
            name="runner-token"
            placeholder="X-Runner-Token for operator routes"
            autocomplete="off"
          />
        </label>
        <p v-if="connectionError" class="instance-connect__error" role="alert">
          {{ connectionError }}
        </p>
        <button class="btn btn--primary" type="submit" :disabled="status === 'connecting'">
          {{ status === "connecting" ? "Connecting…" : "Connect" }}
        </button>
      </form>

      <p class="instance-connect__hint">
        Local dev: start <code>agentium serve</code>, then use <code>http://127.0.0.1:18080</code>
        (Vite on :5173 proxies when <code>VITE_INSTANCE_URL</code> is set).
      </p>
    </div>
  </div>
</template>

<style scoped>
.instance-connect {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: min(70vh, 640px);
  padding: 24px;
}

.instance-connect__card {
  width: min(440px, 100%);
  padding: 28px 24px;
}

.instance-connect__title {
  margin: 0 0 8px;
  font-size: var(--text-xl);
  font-weight: 600;
}

.instance-connect__lede {
  margin: 0 0 20px;
  color: var(--text-secondary);
  font-size: var(--text-md);
  line-height: 1.5;
}

.instance-connect__form {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.instance-connect__field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.instance-connect__label {
  font-size: var(--text-sm);
  font-weight: 500;
  color: var(--text-secondary);
}

.instance-connect__error {
  margin: 0;
  color: var(--color-error);
  font-size: var(--text-md);
}

.instance-connect__hint {
  margin: 16px 0 0;
  font-size: var(--text-sm);
  color: var(--text-muted);
  line-height: 1.45;
}

.instance-connect__hint code {
  font-family: var(--font-mono);
  font-size: var(--text-sm);
}
</style>
