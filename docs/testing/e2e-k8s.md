# E2E K8s Test Harness

End-to-end tests that exercise the Kubernetes deployment feature on a real k3d cluster. These tests cover the kubernetes surface that the Rust-level cluster tests (`runner_cluster_test.rs`) cannot reach: pod DNS resolution, PVC persistence across restarts, kubelet probe behaviour, StatefulSet lifecycle, and kubectl-driven operations.

## Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| Docker **or** Podman | recent | Podman requires rootful mode (see below) |
| k3d | >= 5.x | [k3d.io](https://k3d.io) |
| kubectl | >= 1.28 | Must be able to reach k3d clusters |
| jq | any | JSON processing for assertions |
| curl | any | HTTP requests to pod surfaces |
| Rust toolchain | nightly (pinned via `rust-toolchain.toml`) | Builds the agent builder binary |
| Node.js | >= 22 | Required by the builder for TypeScript compilation |

## Running

```bash
just e2e-k8s
```

Or directly:

```bash
./scripts/e2e-k8s/run.sh
```

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

## Interpreting failures

On any scenario failure, the harness dumps diagnostic data before tearing down:

```
./e2e-k8s-logs/<timestamp>/
├── runner-0.log              # Current pod logs
├── runner-0-previous.log     # Previous container logs (if pod restarted)
├── runner-1.log
├── runner-1-previous.log
├── surrealdb-0.log
├── surrealdb-0-previous.log
├── cluster_runners.json      # SurrealDB cluster registry dump
└── cluster_agent_placements.json
```

### SurrealDB introspection

To query the cluster registry manually while the cluster is running (with `--keep-cluster`):

```bash
kubectl exec -n agentium surrealdb-0 -c surrealdb -- \
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
```

Check for orphaned port-forwards:

```bash
ps aux | grep 'port-forward.*agentium' | grep -v grep | awk '{print $2}' | xargs kill
```
