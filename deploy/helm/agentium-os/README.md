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
  --from-literal=password=$(openssl rand -hex 32)

# Runner operator token
kubectl create secret generic runner-token -n agentium \
  --from-literal=token=$(openssl rand -hex 32)

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

## Runner image

There is no published Agentium runner image. `runner.image.repository` and `runner.image.tag` are required values with no defaults. You must build and push the runner image to your own registry:

```bash
docker build -t your-registry.example.com/agentium-runner:0.1.0 .
docker push your-registry.example.com/agentium-runner:0.1.0
```

## What this chart does not cover

- **Ingress / TLS**: operator access is via `kubectl port-forward` to the API ClusterIP service. Ingress and TLS termination are out of scope for the pilot.
- **Remote config persistence**: config changes do not yet survive pod restarts in remote SurrealDB mode. Tracked in #222.
- **Full operator runbook**: a complete first-run guide is tracked in #224.
- **E2E harness convergence**: the test harness still uses raw manifests. Tracked in #225.
