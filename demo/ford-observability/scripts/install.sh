#!/usr/bin/env bash
# Install (or upgrade) the ford-observability demo via Helm.
#
# Env knobs:
#   NAMESPACE        target namespace (default: agentium-demo)
#   RELEASE          helm release name (default: agentium-observability-demo)
#   CHART            path to chart (default: <demo>/helm)
#   VALUES_FILE      optional extra -f values file
#   ENV_FILE         optional .env file to source before Helm (default: repo .env if present)
#   LOAD_ENV_FILE    1 to source ENV_FILE/default .env, 0 to skip (default: 1)
#   AUTO_VALUES_FROM_ENV
#                    1 to map env secrets into a temp Helm values file (default: 1)
#   WAIT_ROLLOUTS    1 to block on rollout status, 0 to skip (default: 1)
#   ROLLOUT_TIMEOUT  kubectl rollout timeout (default: 5m)
#   HELM_TIMEOUT     helm upgrade --timeout (default: 25m; covers slow agent-deployer hook)
#
# Extra args are forwarded to `helm upgrade --install`, e.g.:
#   ./install.sh --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$DEMO_DIR/../.." && pwd)"

NAMESPACE="${NAMESPACE:-agentium-demo}"
RELEASE="${RELEASE:-agentium-observability-demo}"
CHART="${CHART:-$DEMO_DIR/helm}"
WAIT_ROLLOUTS="${WAIT_ROLLOUTS:-1}"
ROLLOUT_TIMEOUT="${ROLLOUT_TIMEOUT:-5m}"
HELM_TIMEOUT="${HELM_TIMEOUT:-25m}"
LOAD_ENV_FILE="${LOAD_ENV_FILE:-1}"
AUTO_VALUES_FROM_ENV="${AUTO_VALUES_FROM_ENV:-1}"
ENV_FILE="${ENV_FILE:-$REPO_ROOT/.env}"

if [[ "$LOAD_ENV_FILE" == "1" && -f "$ENV_FILE" ]]; then
  echo "[install] loading env from $ENV_FILE (existing shell env wins)"
  env_keys=(
    OPENROUTER_API_KEY
    GRAFANA_API_KEY
    GRAFANA_API_TOKEN
    GRAFANA_PASSWORD
    SLACK_BOT_TOKEN
    SLACK_USER_TOKEN
    SLACK_NOTIFY_CHANNEL_ID
    RUNNER_TOKEN
  )
  declare -A env_was_set=()
  declare -A env_original=()
  for key in "${env_keys[@]}"; do
    if [[ -v "$key" ]]; then
      env_was_set["$key"]=1
      env_original["$key"]="${!key}"
    fi
  done
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
  for key in "${env_keys[@]}"; do
    if [[ "${env_was_set[$key]:-0}" == "1" ]]; then
      printf -v "$key" '%s' "${env_original[$key]}"
      export "$key"
    fi
  done
fi

tmp_values=""
cleanup() {
  if [[ -n "$tmp_values" && -f "$tmp_values" ]]; then
    rm -f "$tmp_values"
  fi
}
trap cleanup EXIT

# Older chart revisions installed cluster-scoped Alloy RBAC. Namespace delete does not
# remove those objects; leftover ClusterRoleBinding trips Helm 4 apply on reinstall.
if kubectl get clusterrolebinding alloy >/dev/null 2>&1 || kubectl get clusterrole alloy >/dev/null 2>&1; then
  echo "[install] removing legacy cluster-scoped alloy RBAC from prior chart revisions"
  kubectl delete clusterrole,clusterrolebinding alloy --ignore-not-found
fi

helm_args=(upgrade --install "$RELEASE" "$CHART"
  --namespace "$NAMESPACE"
  --create-namespace
  --timeout "$HELM_TIMEOUT"
  # Helm 4 defaults to server-side apply, which fails on this chart's cluster-scoped
  # RBAC (ClusterRole/ClusterRoleBinding) against k3s and causes SSA self-conflicts
  # on StatefulSet/DaemonSet upgrades. Client-side apply matches Helm 3 behaviour.
  --server-side=false)

if [[ -n "${VALUES_FILE:-}" ]]; then
  helm_args+=(-f "$VALUES_FILE")
fi

if [[ "$AUTO_VALUES_FROM_ENV" == "1" ]]; then
  if [[ -n "${OPENROUTER_API_KEY:-}${GRAFANA_API_KEY:-}${GRAFANA_API_TOKEN:-}${SLACK_BOT_TOKEN:-}${SLACK_USER_TOKEN:-}${SLACK_NOTIFY_CHANNEL_ID:-}${RUNNER_TOKEN:-}" ]]; then
    if ! command -v jq >/dev/null 2>&1; then
      echo "[install] jq required for AUTO_VALUES_FROM_ENV=1" >&2
      exit 1
    fi
    tmp_values="$(mktemp)"
    jq -n '
      def present($v): ($v != null and $v != "");
      {}
      | if present(env.OPENROUTER_API_KEY) then .secrets.openrouterApiKey = env.OPENROUTER_API_KEY else . end
      | if present(env.GRAFANA_API_KEY) then .secrets.grafanaApiKey = env.GRAFANA_API_KEY elif present(env.GRAFANA_API_TOKEN) then .secrets.grafanaApiToken = env.GRAFANA_API_TOKEN else . end
      | if present(env.GRAFANA_PASSWORD) then .secrets.grafanaAdminPassword = env.GRAFANA_PASSWORD else . end
      | if present(env.SLACK_BOT_TOKEN) then .secrets.slackBotToken = env.SLACK_BOT_TOKEN else . end
      | if present(env.SLACK_USER_TOKEN) then .secrets.slackUserToken = env.SLACK_USER_TOKEN else . end
      | if present(env.SLACK_NOTIFY_CHANNEL_ID) then .secrets.slackNotifyChannelId = env.SLACK_NOTIFY_CHANNEL_ID else . end
      | if present(env.RUNNER_TOKEN) then .agentiumRunner.runnerToken = env.RUNNER_TOKEN else . end
    ' > "$tmp_values"
    helm_args+=(-f "$tmp_values")
    echo "[install] mapped secrets from environment into temporary Helm values"
  fi
fi

echo "[install] helm ${helm_args[*]} $*"
helm "${helm_args[@]}" "$@"

if [[ "$WAIT_ROLLOUTS" != "1" ]]; then
  exit 0
fi

workloads=(
  checkout-api
  payments-api
  failure-harness
  grafana
  prometheus
  loki
  alloy
  k6-load-generator
  agentium-runner
)

echo "[install] waiting for rollouts (timeout=$ROLLOUT_TIMEOUT)"
for w in "${workloads[@]}"; do
  if kubectl -n "$NAMESPACE" get deploy "$w" >/dev/null 2>&1; then
    kubectl -n "$NAMESPACE" rollout status "deploy/$w" --timeout="$ROLLOUT_TIMEOUT"
  elif kubectl -n "$NAMESPACE" get statefulset "$w" >/dev/null 2>&1; then
    kubectl -n "$NAMESPACE" rollout status "statefulset/$w" --timeout="$ROLLOUT_TIMEOUT"
  else
    echo "[install] skip $w (not found in $NAMESPACE)"
  fi
done

echo "[install] done. namespace=$NAMESPACE release=$RELEASE"
