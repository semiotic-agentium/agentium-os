#!/usr/bin/env bash
# Agentium OS k3d demo — spins up a local k8s cluster with shared SurrealDB
# and two runner pods for testing multi-runner agent portability.
#
# ===========================================================================
# DEMO-ONLY LOCAL BOOTSTRAP. NOT the supported install contract.
#
# This script applies raw manifests under deploy/k8s/ and injects keys from
# a .env file — it predates the Helm chart and exists only for ad-hoc local
# exploration.
#
# For the supported install path see:
#   - docs/runbooks/k8s-pilot-operator-guide.md     — operator first-run flow
#   - deploy/helm/agentium-os/README.md    — chart contract
#   - scripts/verify-k8s-pilot-package.sh  — in-repo mirror of the
#                                             operator flow (`just
#                                             verify-k8s-pilot-package`)
# ===========================================================================
#
# Prerequisites:
#   - Docker Desktop or Podman (rootful mode)
#   - k3d (https://k3d.io)
#   - kubectl
#
# macOS with Podman:
#   Podman Machine needs rootful mode and enough RAM for the release build:
#     podman machine stop
#     podman machine set --rootful --memory 8192
#     podman machine start
#   Then set the log driver inside the VM (persists until VM is recreated):
#     podman machine ssh -- 'sudo mkdir -p /etc/containers && \
#       echo -e "[containers]\nlog_driver = \"k8s-file\"" | \
#       sudo tee /etc/containers/containers.conf'
#
# Usage:
#   cp deploy/k8s/surrealdb-credentials.yaml.example deploy/k8s/surrealdb-credentials.yaml
#   cp deploy/k8s/secret-fnox.yaml.example deploy/k8s/secret-fnox.yaml
#   # edit with real values
#   ./deploy/demo/run-demo.sh
#
# Teardown: k3d cluster delete agentium

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CLUSTER_NAME="agentium"
IMAGE_NAME="agentium-runner"
IMAGE_TAG="demo"

echo "=== Agentium OS k3d Demo ==="
echo ""

# Preflight: Podman needs rootful mode, k8s-file log driver, and enough RAM.
if docker info 2>/dev/null | grep -qi podman; then
    if podman machine inspect 2>/dev/null | grep -q '"Rootful": false'; then
        echo "ERROR: Podman Machine is running in rootless mode."
        echo "  Fix: podman machine stop && podman machine set --rootful --memory 8192 && podman machine start"
        exit 1
    fi
    LOG_DRIVER="$(podman info --format '{{.Host.LogDriver}}' 2>/dev/null || true)"
    if [[ "$LOG_DRIVER" == "journald" ]]; then
        echo "ERROR: Podman log driver is 'journald' — k3d needs 'k8s-file'."
        echo "  Fix: podman machine ssh -- 'sudo mkdir -p /etc/containers && echo -e \"[containers]\nlog_driver = \\\"k8s-file\\\"\" | sudo tee /etc/containers/containers.conf'"
        exit 1
    fi
fi

# Preflight: ensure secret files exist (copy from *.example templates).
for secret in surrealdb-credentials.yaml secret-fnox.yaml; do
    if [ ! -f "$REPO_ROOT/deploy/k8s/$secret" ]; then
        echo "ERROR: $REPO_ROOT/deploy/k8s/$secret not found."
        echo "  Copy from the template and fill in your values:"
        echo "    cp $REPO_ROOT/deploy/k8s/${secret}.example $REPO_ROOT/deploy/k8s/$secret"
        exit 1
    fi
done

# Safety: verify kube context points at a k3d cluster to prevent accidentally
# pushing secrets to a non-local environment.
CURRENT_CTX="$(kubectl config current-context 2>/dev/null || true)"
if [[ "$CURRENT_CTX" != k3d-* ]]; then
    if k3d cluster list -o json 2>/dev/null | grep -q "\"name\":\"$CLUSTER_NAME\""; then
        echo "ERROR: current kube context is '$CURRENT_CTX', expected a k3d-* context."
        echo "  The cluster exists — run: kubectl config use-context k3d-$CLUSTER_NAME"
        exit 1
    fi
    echo "No k3d context set — step 1 will create the cluster and set it."
fi

# 1. Build container image (before cluster — release link needs most of the VM RAM).
echo "[1/8] Building container image..."
docker build -t "$IMAGE_NAME:$IMAGE_TAG" "$REPO_ROOT"

# 2. Create k3d cluster
echo "[2/8] Creating k3d cluster..."
if k3d cluster list -o json 2>/dev/null | grep -q "\"name\":\"$CLUSTER_NAME\""; then
    echo "  Cluster '$CLUSTER_NAME' already exists, reusing."
else
    k3d cluster create --config "$REPO_ROOT/deploy/k3d/cluster.yaml"
fi

# 3. Import image into k3d (save to tarball first — k3d cannot read
#    Podman's image store directly).
echo "[3/8] Importing image into k3d..."
IMPORT_TAR="$(mktemp -t agentium-image-XXXXXX).tar"
# Podman stores images as localhost/<name>; retag so the name matches runner.yaml.
docker tag "$IMAGE_NAME:$IMAGE_TAG" "docker.io/library/$IMAGE_NAME:$IMAGE_TAG" 2>/dev/null || true
docker save -o "$IMPORT_TAR" "docker.io/library/$IMAGE_NAME:$IMAGE_TAG"
k3d image import "$IMPORT_TAR" -c "$CLUSTER_NAME"
rm -f "$IMPORT_TAR"

# 4. Apply k8s manifests
echo "[4/9] Applying k8s manifests..."
kubectl apply -f "$REPO_ROOT/deploy/k8s/namespace.yaml"
kubectl apply -f "$REPO_ROOT/deploy/k8s/surrealdb-credentials.yaml"
kubectl apply -f "$REPO_ROOT/deploy/k8s/surrealdb.yaml"
kubectl apply -f "$REPO_ROOT/deploy/k8s/secret-fnox.yaml"

# Generate runner-token secret if it does not already exist in the cluster.
if ! kubectl -n agentium get secret runner-token >/dev/null 2>&1; then
    RUNNER_TOKEN="$(openssl rand -hex 32)"
    kubectl -n agentium create secret generic runner-token --from-literal=token="$RUNNER_TOKEN"
    echo "  Generated runner-token secret (token saved for later output)."
else
    RUNNER_TOKEN=""
    echo "  runner-token secret already exists, reusing."
fi

kubectl apply -f "$REPO_ROOT/deploy/k8s/runner.yaml"
kubectl apply -f "$REPO_ROOT/deploy/k8s/networkpolicy.yaml"

# 5. Wait for readiness
echo "[5/9] Waiting for pods to be ready..."
kubectl -n agentium wait --for=condition=ready pod -l app=surrealdb --timeout=120s
kubectl -n agentium wait --for=condition=ready pod -l app=runner --timeout=120s

echo "[6/9] Runners ready. Listing pods:"
kubectl -n agentium get pods -o wide

# 7. Port-forward runner-0 for demo interaction
echo "[7/9] Port-forwarding runner-0 to localhost:18080..."
kubectl -n agentium port-forward runner-0 18080:18080 &
PF_PID=$!
trap 'kill $PF_PID 2>/dev/null' EXIT INT TERM
sleep 2

# 8. Retrieve the runner token for usage output (if not freshly generated).
if [ -z "$RUNNER_TOKEN" ]; then
    RUNNER_TOKEN="$(kubectl -n agentium get secret runner-token -o jsonpath='{.data.token}' | base64 -d 2>/dev/null || true)"
fi

echo "[8/9] Runner token configured."
echo "[9/9] Demo environment ready."
echo ""
echo "  runner-0: http://localhost:18080"
echo ""
echo "  Public routes (no auth required):"
echo "    curl http://localhost:18080/healthz"
echo "    curl http://localhost:18080/readyz"
echo "    curl http://localhost:18080/agents"
echo ""
echo "  Operator routes (require X-Runner-Token):"
echo "    curl -H 'X-Runner-Token: $RUNNER_TOKEN' http://localhost:18080/config"
echo "    curl -H 'X-Runner-Token: $RUNNER_TOKEN' http://localhost:18080/config/secrets-overview"
echo "    curl -X POST -H 'X-Runner-Token: $RUNNER_TOKEN' -H 'Content-Type: application/json' \\"
echo "         -d '{\"hash\":\"<content_hash>\"}' http://localhost:18080/deploy"
echo ""
echo "To clean up: k3d cluster delete $CLUSTER_NAME"
echo "Port-forward PID: $PF_PID (kill $PF_PID to stop)"
