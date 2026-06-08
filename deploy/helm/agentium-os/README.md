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

### Local k3d (Argo CD + local registry)

In-repo local validation uses Argo CD and a nonce image tag:

```bash
just up
# or: just verify-k8s-pilot-package
```

Values: [`deploy/values/local/defaults.yaml`](../../values/local/defaults.yaml) plus generated [`deploy/values/generated/images.yaml`](../../values/generated/images.yaml). See [`deploy/argocd/README.md`](../../argocd/README.md) and [`RELEASING.md`](../../../RELEASING.md).

The host pushes to `localhost:5400`; the cluster pulls from `k3d-agentium-registry:5000` (`deploy/k3d/cluster.yaml`).

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

For a scripted, end-to-end local validation of this same install path (cluster → three required objects → `helm upgrade --install` → smoke → cluster_runners verify), run `just verify-k8s-pilot-package` (see [`docs/runbooks/e2e-k8s.md`](../../../docs/runbooks/e2e-k8s.md)).

## Runner image

There is no published Agentium runner image. `runner.image.repository` and `runner.image.tag` are required values with no defaults. You must build and push the runner image to your own registry:

```bash
docker build -t your-registry.example.com/agentium-runner:0.1.0 .
docker push your-registry.example.com/agentium-runner:0.1.0
```

For any real design-partner or shared-cluster install, that means supplying a cluster-reachable OCI image reference, typically via a private registry. The `k3d image import` flow above is a fast dev fallback for local iteration; the k3d-managed-registry flow above mirrors the real install contract end-to-end.

### Image pull policy

The chart sets `runner.image.pullPolicy: Always` by default. The pilot install posture has no published runner image: every operator builds and pushes their own, and same-tag rebuilds are the common case. Under `IfNotPresent`, the kubelet silently reuses the layer cached on the node after a `kubectl rollout restart`, so a fresh push to the same tag is invisible to the running pod.

If you pin to immutable tags (or content digests) and want to avoid the per-restart registry round-trip, override `runner.image.pullPolicy: IfNotPresent` in your values file. The `k3d-values.yaml` image-import example overrides to `Never` because that flow bypasses the kubelet pull entirely.

## Next step

For the full first-run operator flow (including building the runner image, creating the required objects, authenticated publish/deploy, and the packaged smoke script), see [`docs/runbooks/k8s-pilot-operator-guide.md`](../../../docs/runbooks/k8s-pilot-operator-guide.md).

## Runner probes

The runner StatefulSet uses HTTP `GET /healthz` for startup and liveness, and `GET /readyz` for readiness. The chart sets explicit, conservative defaults:

| Knob                  | startupProbe | livenessProbe | readinessProbe |
| --------------------- | ------------ | ------------- | -------------- |
| `initialDelaySeconds` | `20`         | `10`          | `5`            |
| `periodSeconds`       | `10`         | `15`          | `10`           |
| `timeoutSeconds`      | `5`          | `5`           | `5`            |
| `failureThreshold`    | `6`          | `6`           | `6`            |

`timeoutSeconds` and `failureThreshold` are higher than the Kubernetes defaults (`1` and `3`) because `POST /deploy` does meaningful synchronous work (QuickJS init, BAML schema parse, tool wrapping, agent boot) that can starve the HTTP server long enough for tight probes to fail under contention. When a readiness probe fails, the kubelet removes the pod from the runner Service endpoints and any in-flight Service-routed client connection (including `kubectl port-forward svc/...`) is reset mid-deploy.

The startup probe exists because the runner's HTTP listener doesn't bind until after the in-process SurrealDB connect retry has succeeded (issue #381 / PR #396), which on a cold install can take ~15 s while DNS / the SurrealDB pod itself come up. While the startup probe is running, the kubelet suppresses liveness and readiness probes — neither can emit `Warning Unhealthy` events nor restart the pod until startup completes. `initialDelaySeconds: 20` is set just past the typical HTTP-bind time so the first probe succeeds on first attempt during a cold install. Total startup budget = `initialDelaySeconds + failureThreshold * periodSeconds = 80 s` before the kubelet restarts the container, which comfortably covers the SurrealDB retry's worst case.

Override any field via the `runner.startupProbe.*`, `runner.livenessProbe.*`, and `runner.readinessProbe.*` keys in your values file. Probe paths and ports are not overridable — they are part of the runner contract.

## Runner memory

The chart defaults each runner to `2Gi` request and `5Gi` limit. `POST /deploy` is the binding constraint: tar extract, BAML IL load, QuickJS init, and tool registration run on top of the resident fastembed ONNX model, SurrealDB client, and provenance backend, and in-cluster TypeScript compilation during publish is the heaviest single step. 5Gi is the empirically observed floor for publishing a real multi-agent set end-to-end; below that, the runner tends to be `OOMKilled` mid-deploy and clients see `connection closed before message completed` from `POST /deploy`. Raise `runner.resources.limits.memory` further for heavier workloads.

**Local k3d host memory floor.** Two runner replicas (`2Gi` request each) plus SurrealDB (`256Mi` request) account for 4.25Gi of memory requests, and kube-system pods consume more on top. On macOS Docker Desktop (default 4 GiB) or colima with similar defaults, that's below the floor — pods stay `Pending` with `0/1 nodes are available: 1 Insufficient memory`. Allocate **≥6 GiB** to Docker Desktop / colima before installing.

This is a distinct failure mode from the `OOMKilled` path above. `Insufficient memory` happens at scheduling time, before a pod starts; `OOMKilled` happens after a pod is already running and exceeds its limit.

## SurrealDB namespaces

The chart provisions a single SurrealDB instance shared by every runner crate. Each crate owns its own namespace / database pair; **there is no single "agentium" namespace**. When debugging or running ad-hoc queries you must target the right pair:

| Namespace    | Database       | Owner                  | Contents                                            |
| ------------ | -------------- | ---------------------- | --------------------------------------------------- |
| `cluster`    | `registry`     | `baml-agent-runner`    | `cluster_runners`, `cluster_agent_placements`       |
| `provenance` | `store`        | `baml-rt-provenance`   | provenance graph (nodes, edges, payloads, archives) |
| `config`     | `store`        | `baml-rt-config`       | tool configuration and secrets overview             |
| `baml`       | `repository`   | `baml-rt-repository`   | content-addressable agent package archive           |
| `baml`       | `runner_state` | `baml-agent-runner`    | deployment state                                    |

The authoritative list lives at `scripts/e2e-k8s/lib.sh:SURREAL_KNOWN_NAMESPACES`; update it (and this table) when a crate adds a new namespace.

### Querying interactively

```bash
SURREAL_POD="$(kubectl get pod -n agentium -l app.kubernetes.io/component=surrealdb -o jsonpath='{.items[0].metadata.name}')"

# Cluster routing state
kubectl exec -n agentium "$SURREAL_POD" -c surrealdb -i -- \
  /surreal sql --endpoint http://localhost:8000 \
    --username "$SURREAL_USER" --password "$SURREAL_PASS" \
    --namespace cluster --database registry --json \
  <<<"SELECT * FROM cluster_runners;"

# Provenance graph density
kubectl exec -n agentium "$SURREAL_POD" -c surrealdb -i -- \
  /surreal sql --endpoint http://localhost:8000 \
    --username "$SURREAL_USER" --password "$SURREAL_PASS" \
    --namespace provenance --database store --json \
  <<<"SELECT count() FROM prov_node GROUP ALL;"
```

For repeated use, `scripts/e2e-k8s/lib.sh` exposes a `surreal_query <sql> [ns] [db]` helper with a stable output contract and a namespace pre-check that catches typos before they hit SurrealDB.

### Troubleshooting: "Couldn't write to a read only transaction"

If you see this error from `/sql`:

```json
{"kind":"Internal","result":"There was a problem with the key-value store: Couldn't write to a read only transaction","status":"ERR"}
```

…the `Surreal-NS:` header (or `--namespace` flag) is targeting a namespace that doesn't exist. SurrealDB v3 opens a read-only transaction for `SELECT` and can't auto-`DEFINE NAMESPACE` to satisfy the resolution, surfacing the storage-layer error instead of a clearer `NamespaceNotFound`. Check the spelling against the table above. This was issue [#388](https://github.com/semiotic-agentium/agentium-os/issues/388).

## What this chart does not cover

- **Ingress / TLS**: operator access is via `kubectl port-forward` to the API ClusterIP service. Ingress and TLS termination are out of scope for the pilot.
- **Load-test baseline**: performance SLOs for the pilot topology. Tracked in #226.
