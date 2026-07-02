<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useDeployApi } from "../composables/useDeployApi";
import { useConfirm } from "../composables/useConfirm";

const { deployments, loading, error, fetchDeployments, deploy, undeploy } = useDeployApi();
const { confirm } = useConfirm();

const showDeployForm = ref(false);
const deployHash = ref("");
const deployName = ref("");
const deployVersion = ref("");
const deploying = ref(false);

onMounted(() => fetchDeployments());

async function handleDeploy() {
  deploying.value = true;
  const request = deployHash.value
    ? { hash: deployHash.value }
    : { name: deployName.value, version: deployVersion.value || undefined };
  await deploy(request);
  deploying.value = false;
  showDeployForm.value = false;
  deployHash.value = "";
  deployName.value = "";
  deployVersion.value = "";
}

async function handleUndeploy(hash: string, agentName: string) {
  const ok = await confirm(
    "Undeploy agent?",
    `This will undeploy "${agentName}" (${hash.slice(0, 12)}...). The agent will stop serving requests.`,
  );
  if (!ok) return;
  await undeploy(hash);
}

function formatDate(isoStr: string): string {
  try {
    return new Date(isoStr).toLocaleString([], {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return isoStr;
  }
}
</script>

<template>
  <div class="deployment-panel">
    <div class="deployment-header">
      <h3>Deployments</h3>
      <div class="deployment-actions">
        <button class="btn btn--sm btn--secondary" :disabled="loading" @click="fetchDeployments">
          Refresh
        </button>
        <button class="btn btn--sm btn--primary" @click="showDeployForm = !showDeployForm">
          {{ showDeployForm ? "Cancel" : "Deploy" }}
        </button>
      </div>
    </div>

    <div v-if="error" class="deployment-error">{{ error }}</div>

    <div v-if="showDeployForm" class="deploy-form">
      <div class="deploy-form-row">
        <label>Content Hash</label>
        <input
          v-model="deployHash"
          class="input"
          placeholder="sha256:..."
          :disabled="!!deployName"
        />
      </div>
      <div class="deploy-form-divider">— or —</div>
      <div class="deploy-form-row">
        <label>Agent Name</label>
        <input
          v-model="deployName"
          class="input"
          placeholder="my-agent"
          :disabled="!!deployHash"
        />
      </div>
      <div class="deploy-form-row">
        <label>Version</label>
        <input
          v-model="deployVersion"
          class="input"
          placeholder="latest"
          :disabled="!!deployHash"
        />
      </div>
      <button
        class="btn btn--primary"
        :disabled="deploying || (!deployHash && !deployName)"
        @click="handleDeploy"
      >
        {{ deploying ? "Deploying..." : "Deploy" }}
      </button>
    </div>

    <div v-if="loading && deployments.length === 0" class="deployment-empty">Loading...</div>

    <div v-else-if="deployments.length === 0" class="deployment-empty">
      No agents deployed. Use the Deploy button or <code>agentium install agent</code>.
    </div>

    <table v-else class="deployment-table">
      <thead>
        <tr>
          <th>Agent</th>
          <th>Status</th>
          <th>Hash</th>
          <th>Deployed</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="d in deployments" :key="d.content_hash">
          <td class="deploy-agent-name">{{ d.agent_name }}</td>
          <td>
            <span :class="['deploy-status-badge', d.status === 'active' ? 'badge-active' : 'badge-failed']">
              {{ d.status }}
            </span>
            <span v-if="d.failure_count > 0" class="deploy-failure-count">
              ({{ d.failure_count }} failure{{ d.failure_count !== 1 ? "s" : "" }})
            </span>
          </td>
          <td class="deploy-hash" :title="d.content_hash">{{ d.content_hash.slice(0, 16) }}...</td>
          <td>{{ formatDate(d.deployed_at) }}</td>
          <td>
            <button
              class="btn btn--sm btn--ghost"
              title="Undeploy"
              @click="handleUndeploy(d.content_hash, d.agent_name)"
            >
              Undeploy
            </button>
          </td>
        </tr>
      </tbody>
    </table>

    <div v-if="deployments.some(d => d.last_error)" class="deployment-errors-section">
      <h4>Recent Errors</h4>
      <div v-for="d in deployments.filter(d => d.last_error)" :key="d.content_hash" class="deployment-error-item">
        <strong>{{ d.agent_name }}</strong>: {{ d.last_error }}
        <span v-if="d.last_attempt_at" class="deployment-error-time">{{ formatDate(d.last_attempt_at) }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.deployment-panel {
  padding: 16px;
}

.deployment-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}

.deployment-header h3 {
  font-size: var(--text-lg, 16px);
  color: var(--text);
}

.deployment-actions {
  display: flex;
  gap: 8px;
}

.deployment-error {
  padding: 8px 12px;
  background: var(--color-error-subtle);
  border: 1px solid var(--color-error-border);
  border-radius: var(--radius-sm, 8px);
  color: var(--color-error);
  font-size: var(--text-sm, 11px);
  margin-bottom: 12px;
}

.deploy-form {
  background: var(--bg-subtle);
  border: 1px solid var(--border);
  border-radius: var(--radius-md, 12px);
  padding: 16px;
  margin-bottom: 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.deploy-form-row {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.deploy-form-row label {
  font-size: var(--text-sm, 11px);
  color: var(--text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
}

.deploy-form-divider {
  text-align: center;
  font-size: var(--text-sm, 11px);
  color: var(--text-muted);
}

.deployment-empty {
  text-align: center;
  padding: 32px 16px;
  color: var(--text-muted);
  font-size: var(--text-base, 13px);
}

.deployment-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--text-base, 13px);
}

.deployment-table th {
  text-align: left;
  font-size: var(--text-sm, 11px);
  color: var(--text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  padding: 8px 12px;
  border-bottom: 1px solid var(--border);
}

.deployment-table td {
  padding: 10px 12px;
  border-bottom: 1px solid var(--border-subtle);
}

.deploy-agent-name {
  font-weight: 500;
}

.deploy-status-badge {
  display: inline-block;
  padding: 2px 8px;
  border-radius: 10px;
  font-size: var(--text-xs, 10px);
  font-weight: 600;
  text-transform: uppercase;
}

.badge-active {
  background: var(--color-success-subtle);
  color: var(--color-success);
}

.badge-failed {
  background: var(--color-error-subtle);
  color: var(--color-error);
}

.deploy-failure-count {
  font-size: var(--text-xs, 10px);
  color: var(--color-error);
  margin-left: 4px;
}

.deploy-hash {
  font-family: var(--font-mono);
  font-size: var(--text-xs, 10px);
  color: var(--text-muted);
}

.deployment-errors-section {
  margin-top: 16px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}

.deployment-errors-section h4 {
  font-size: var(--text-sm, 11px);
  color: var(--color-error);
  margin-bottom: 8px;
}

.deployment-error-item {
  font-size: var(--text-sm, 11px);
  color: var(--text-secondary);
  padding: 4px 0;
}

.deployment-error-time {
  color: var(--text-muted);
  margin-left: 8px;
}
</style>
