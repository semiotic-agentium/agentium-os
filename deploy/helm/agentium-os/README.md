# Agentium OS Helm Chart

Supported Kubernetes install surface for the Agentium OS pilot. Deploys a two-runner StatefulSet with a shared SurrealDB provenance store in a single namespace.

## Prerequisites

- Kubernetes cluster with a default StorageClass
- Helm 3.x
- `kubectl` configured for the target cluster

## Create secrets and config

The chart references pre-existing Kubernetes secrets and ConfigMaps. Create them before installing.

```bash
kubectl create namespace agentium

# SurrealDB credentials
kubectl create secret generic surrealdb-credentials -n agentium \
  --from-literal=username=agentium \
  --from-literal=password="$(openssl rand -hex 32)"

# Runner operator token
kubectl create secret generic runner-token -n agentium \
  --from-literal=token="$(openssl rand -hex 32)"

# fnox.toml (LLM configuration)
kubectl create configmap fnox-config -n agentium \
  --from-file=fnox.toml=./fnox.toml
```

## Install

```bash
helm upgrade --install agentium deploy/helm/agentium-os/ \
  --namespace agentium --create-namespace \
  -f deploy/helm/agentium-os/examples/design-partner-values.yaml
```

For local k3d development, build the runner image and import it into the k3d cluster, then install with the k3d example values:

```bash
# Build the runner image (from repo root)
docker build -t agentium-runner:demo .

# Import into k3d (assumes cluster name "agentium")
k3d image import agentium-runner:demo -c agentium

# Install
helm upgrade --install agentium deploy/helm/agentium-os/ \
  --namespace agentium --create-namespace \
  -f deploy/helm/agentium-os/examples/k3d-values.yaml
```

## Verify

```bash
# Port-forward to the runner API service
kubectl port-forward svc/agentium-agentium-os-runner-api 18080:18080 -n agentium

# Health check
curl http://localhost:18080/healthz
curl http://localhost:18080/readyz

# List deployed agents (requires X-Runner-Token for operator routes)
curl http://localhost:18080/agents
```

For a scripted, end-to-end local validation of this same install path (cluster → three required objects → `helm upgrade --install` → smoke → cluster_runners verify), run `just verify-k8s-pilot-package` (see [`docs/testing/e2e-k8s.md`](../../../docs/testing/e2e-k8s.md)).

## Runner image

There is no published Agentium runner image. `runner.image.repository` and `runner.image.tag` are required values with no defaults. You must build and push the runner image to your own registry:

```bash
docker build -t your-registry.example.com/agentium-runner:0.1.0 .
docker push your-registry.example.com/agentium-runner:0.1.0
```

For any real design-partner or shared-cluster install, that means supplying a cluster-reachable OCI image reference, typically via a private registry. The `k3d image import` flow above is a local-development exception only.

## Next step

For the full first-run operator flow (including building the runner image, creating the required objects, authenticated publish/deploy, and the packaged smoke script), see [`docs/k8s-pilot-operator-guide.md`](../../../docs/k8s-pilot-operator-guide.md).

## What this chart does not cover

- **Ingress / TLS**: operator access is via `kubectl port-forward` to the API ClusterIP service. Ingress and TLS termination are out of scope for the pilot.
- **Load-test baseline**: performance SLOs for the pilot topology. Tracked in #226.
