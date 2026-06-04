# Kubernetes Package Validation

The repo has three in-repo validation paths for the Kubernetes pilot:

1. **Authoritative package-validation flow** — `scripts/verify-k8s-pilot-package.sh`.
   Brings up k3d, pushes the runner image to the local registry, creates
   required secrets/configmaps, installs via **Argo CD sync**
   (`install_pilot_via_argo` in `scripts/e2e-k8s/lib.sh`), runs
   `scripts/k8s-pilot-smoke.sh`, and verifies `cluster_runners`.
2. **Richer scenario coverage** — `scripts/e2e-k8s/run.sh`. 15 scenarios
   on the same Argo-managed topology.
3. **Cgroup-throttled deploy** — `scripts/e2e-k8s/t2-cgroup-throttle.sh`.

All three paths use the shared bringup helpers in `scripts/e2e-k8s/lib.sh`
(`ensure_runner_image_available`, `create_pilot_objects`,
`install_pilot_via_argo`, `resolve_chart_names`, `wait_for_runner_readyz`).

**Local dev entry:** `just up` (k3d + nonce tag + Argo sync).

The raw manifests under `deploy/k8s/` are legacy assets and are **not**
the supported install surface.

## Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| Docker **or** Podman | recent | Podman requires rootful mode (see below) |
| k3d | >= 5.x | [k3d.io](https://k3d.io) |
| kubectl | >= 1.28 | Must be able to reach k3d clusters |
| helm | >= 3.x | Installs the supported chart |
| jq | any | JSON processing for assertions |
| curl | any | HTTP requests to pod surfaces |
| Rust toolchain | nightly (pinned via `rust-toolchain.toml`) | Builds the agent builder binary |
| Node.js | >= 22 | Required by the builder for TypeScript compilation |

## Running

### Authoritative package validation

```bash
just verify-k8s-pilot-package
```

Or directly:

```bash
./scripts/verify-k8s-pilot-package.sh
```

Flags: `--no-build`, `--keep-cluster`, `--image-tag`, `--local-port`, `--values`. See `--help`.

Image tags are nonces from `deploy/values/generated/.last-image-tag` (or `AGENTIUM_IMAGE_TAG`). The runner image is pushed to `k3d-agentium-registry:5000` and installed via Argo CD.

### Richer scenario suite

```bash
just e2e-k8s
```

Or directly:

```bash
./scripts/e2e-k8s/run.sh
```

This is the same Helm topology the authoritative flow installs, plus 15
scenario assertions (listed below).

### Options

| Flag | Effect |
|------|--------|
| `--no-build` | Skip Docker image and builder binary builds (reuse cached artifacts) |
| `--keep-cluster` | Leave the k3d cluster running after tests complete |

A typical first run takes 5-10 minutes (Docker image build is the long pole). Subsequent runs with `--no-build` complete in 2-4 minutes.

## Podman caveats

If using Podman instead of Docker Desktop:

1. **Rootful mode** — Podman Machine must run in rootful mode with enough RAM for the Rust release build:

   ```bash
   podman machine stop
   podman machine set --rootful --memory 8192
   podman machine start
   ```

2. **Log driver** — k3d requires the `k8s-file` log driver, not the default `journald`:

   ```bash
   podman machine ssh -- 'sudo mkdir -p /etc/containers && \
     echo -e "[containers]\nlog_driver = \"k8s-file\"" | \
     sudo tee /etc/containers/containers.conf'
   ```

The harness checks both conditions at startup and exits with a clear message if either is misconfigured.

## Scenarios

| # | Name | What it verifies |
|---|------|------------------|
| 1 | Cluster boot sanity | Both runners reach Ready, probes return 200, `cluster_runners` has 2 rows with correct pod DNS endpoints |
| 2 | PVC persistence | Published agent data survives pod deletion and recreation (StatefulSet PVC reattachment) |
| 3 | Cross-pod A2A | A2A request to runner-0 for an agent deployed only on runner-1 is forwarded via cluster placement resolution |
| 4 | Cross-pod migration | `/control/migrate` moves an agent from runner-0 to runner-1; placement table updated |
| 5 | SSRF rejection | Migrate with loopback, link-local, and cloud metadata targets returns 4xx; agent remains deployed |
| 6 | Token enforcement | Deploy without token and with wrong token returns 401; correct token returns 200 |
| 7 | Graceful drain | In-flight request during pod termination is handled gracefully; AgentStopped provenance event emitted |
| 8 | Heartbeat advances | `last_heartbeat_ms` in `cluster_runners` advances by at least 5s over a 12s window |
| 9 | Readyz 503 window | New pod returns 503 on `/readyz` during initialization before transitioning to 200 |
| 10 | Distributed multi-agent conversation | Cleese calls Chapman cross-pod via LLM-driven `internal_a2a`; full conversation flows through the cluster mesh |
| 11 | Full agent lifecycle | One agent, one hash: publish → deploy → use → migrate → transparent forward → undeploy → redeploy |
| 12 | Provenance survives migration | Audit trail follows the agent across infrastructure; both runners see identical provenance from shared SurrealDB |
| 13 | Stale runner exclusion | Force-killed runner excluded from placement routing within heartbeat TTL; recovers automatically on restart |
| 14 | Concurrent deployment convergence | Same agent deployed on both runners simultaneously; each serves locally, placement converges |
| 15 | Task lifecycle across pods | Multi-turn conversation with INPUT_REQUIRED; documents behavior when agent migrates mid-conversation |

Scenario 10 is the harness version of the Cleese/Chapman product story. It is LLM-dependent and only exercises the full cross-pod conversation when the installed cluster's `fnox-config` supplies `OPENROUTER_API_KEY`. For the narrower operator-facing validation path on a Helm-installed pilot, use [`scripts/k8s-pilot-cleese-chapman.sh`](../scripts/k8s-pilot-cleese-chapman.sh) after following [`docs/runbooks/k8s-pilot-operator-guide.md`](k8s-pilot-operator-guide.md).

## Interpreting failures

On any scenario failure, the harness dumps diagnostic data before tearing down:

```
./e2e-k8s-logs/<timestamp>/
├── agentium-agentium-os-runner-0.log            # Current pod logs
├── agentium-agentium-os-runner-0-previous.log   # Previous container logs (if pod restarted)
├── agentium-agentium-os-runner-0-tail.log       # Tail re-captured ~2s after the dump (catches lines emitted post-failure)
├── agentium-agentium-os-runner-1.log
├── agentium-agentium-os-runner-1-previous.log
├── agentium-agentium-os-runner-1-tail.log
├── agentium-agentium-os-surrealdb-0.log
├── agentium-agentium-os-surrealdb-0-previous.log
├── describe-pods.txt                            # `kubectl describe pod` for each chart pod
├── events.txt                                   # Namespace events sorted by lastTimestamp
├── all-pods.txt                                 # `kubectl get pods -A -o wide`
├── cluster-state.yaml                           # StatefulSet/Service state in the namespace
├── configmaps-list.txt                          # ConfigMap *names* (no data — fnox-config carries LLM keys)
├── secrets-list.txt                             # Secret *names* (no values — never captured)
├── port-forward.log                             # `kubectl port-forward` log when smoke ran far enough to start it
├── cluster_runners.json                         # SurrealDB cluster registry dump (see status field)
└── cluster_agent_placements.json
```

File names reflect the chart-rendered StatefulSet names
(`<release>-agentium-os-runner`, `<release>-agentium-os-surrealdb`). The
default release is `agentium`.

When triaging a failed lane, work from cluster layer down to runner layer:

| Layer | File | What it answers |
|-------|------|-----------------|
| Scheduling / image pull / OOM / eviction | `describe-pods.txt` | Why a pod is not Running, last container exit reason, ImagePullBackOff details |
| NetworkPolicy drops, scheduling failures, kubelet rejections | `events.txt` | Cluster-side events the runner never sees |
| Cluster-wide pod state | `all-pods.txt` | Whether expected pods exist at all, on which node, with what age |
| Chart wiring | `cluster-state.yaml` | StatefulSets and Services actually rendered (replica count, image, env, mounts) |
| Operator transport | `port-forward.log` | Whether `kubectl port-forward` died, was reset, or never bound |
| Runner request handling | `*-tail.log` | Lines emitted in the seconds **after** the original log dump — usually the panic / 5xx body |
| Runner startup / steady state | `*.log`, `*-previous.log` | Full per-container log; `--previous` covers the pre-restart container if pod looped |
| Cluster state in SurrealDB | `cluster_runners.json`, `cluster_agent_placements.json` | Whether runners registered and where agents are placed |

`cluster_runners.json` and `cluster_agent_placements.json` carry a
top-level `_query_status` field (`ok` or `failed`); on failure an
`_error` field gives the underlying SurrealDB / `kubectl exec` message.
This means an empty `result` array unambiguously says "table is empty"
rather than "the query never reached SurrealDB".

ConfigMap and Secret *data* are deliberately never written to
artifacts; only their names are captured (`configmaps-list.txt`,
`secrets-list.txt`). The `fnox-config` ConfigMap is built from
`fnox.toml` and carries `default = "..."` LLM API keys when the repo
root has a populated fnox file, so `-o yaml` on either resource would
leak credentials into the artifact zip.

### SurrealDB introspection

To query the cluster registry manually while the cluster is running (with `--keep-cluster`), use the chart-rendered SurrealDB pod name:

```bash
SURREAL_POD=$(kubectl -n agentium get statefulset \
  -l app.kubernetes.io/instance=agentium,app.kubernetes.io/component=surrealdb \
  -o jsonpath='{.items[0].metadata.name}')-0

kubectl exec -n agentium "$SURREAL_POD" -c surrealdb -- \
  /surreal sql \
  --endpoint http://localhost:8000 \
  --username e2e --password e2e-test-pass \
  --namespace cluster --database registry \
  --json \
  "SELECT * FROM cluster_runners"
```

Replace the SQL statement as needed. The provenance store uses namespace `provenance`, database `store`.

## Manual cleanup

If the harness crashes without running its trap handler (e.g. `kill -9`), clean up manually:

```bash
k3d cluster delete agentium

# Optional: also remove the local registry container (persists across runs
# with --restart=unless-stopped so it stays available for future
# with --keep-cluster).
docker rm -f k3d-agentium-registry
```

Check for orphaned port-forwards:

```bash
ps aux | grep 'port-forward.*agentium' | grep -v grep | awk '{print $2}' | xargs kill
```
