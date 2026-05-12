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

### Local k3d (k3d-managed registry)

`scripts/verify-k8s-pilot-package.sh --image-strategy registry` exercises
the same kubelet pull contract as a real registry-backed install by
pushing the runner image to a k3d-managed local registry encoded in
`deploy/k3d/cluster.yaml`:

```bash
docker build -t agentium-runner:demo .
./scripts/verify-k8s-pilot-package.sh \
  --image-strategy registry \
  --image-repository agentium-runner --image-tag demo
```

The host pushes to `localhost:5400` and the cluster pulls from
`k3d-agentium-registry:5000`. No external registry is required. The
chart is installed with
[`examples/k3d-registry-values.yaml`](examples/k3d-registry-values.yaml).

### Local k3d (image import — fast dev fallback)

For quick iteration without exercising the kubelet pull path, build the
runner image and import it into the k3d cluster, then install with the
k3d example values:

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

For any real design-partner or shared-cluster install, that means supplying a cluster-reachable OCI image reference, typically via a private registry. The `k3d image import` flow above is a fast dev fallback for local iteration; the k3d-managed-registry flow above mirrors the real install contract end-to-end.

## Next step

For the full first-run operator flow (including building the runner image, creating the required objects, authenticated publish/deploy, and the packaged smoke script), see [`docs/k8s-pilot-operator-guide.md`](../../../docs/k8s-pilot-operator-guide.md).

## Runner probes

The runner StatefulSet uses HTTP `GET /healthz` for liveness and `GET /readyz` for readiness. The chart sets explicit, conservative defaults:

| Knob                  | livenessProbe | readinessProbe |
| --------------------- | ------------- | -------------- |
| `initialDelaySeconds` | `10`          | `5`            |
| `periodSeconds`       | `15`          | `10`           |
| `timeoutSeconds`      | `5`           | `5`            |
| `failureThreshold`    | `6`           | `6`            |

`timeoutSeconds` and `failureThreshold` are higher than the Kubernetes defaults (`1` and `3`) because `POST /deploy` does meaningful synchronous work (QuickJS init, BAML schema parse, tool wrapping, agent boot) that can starve the HTTP server long enough for tight probes to fail under contention. When a readiness probe fails, the kubelet removes the pod from the runner Service endpoints and any in-flight Service-routed client connection (including `kubectl port-forward svc/...`) is reset mid-deploy.

Override any field via the `runner.livenessProbe.*` and `runner.readinessProbe.*` keys in your values file. Probe paths and ports are not overridable — they are part of the runner contract.

## Runner memory

The chart defaults each runner to `2Gi` request and `5Gi` limit. `POST /deploy` is the binding constraint: tar extract, BAML IL load, QuickJS init, and tool registration run on top of the resident fastembed ONNX model, SurrealDB client, and provenance backend, and in-cluster TypeScript compilation during publish is the heaviest single step. 5Gi is the empirically observed floor for publishing a real multi-agent set end-to-end; below that, the runner tends to be `OOMKilled` mid-deploy and clients see `connection closed before message completed` from `POST /deploy`. Raise `runner.resources.limits.memory` further for heavier workloads.

## What this chart does not cover

- **Ingress / TLS**: operator access is via `kubectl port-forward` to the API ClusterIP service. Ingress and TLS termination are out of scope for the pilot.
- **Load-test baseline**: performance SLOs for the pilot topology. Tracked in #226.
