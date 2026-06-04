# deploy/k8s/ — Legacy Manifests

These raw Kubernetes manifests are **not** the supported install surface.

For local k3d development use `just up` (Argo CD + local registry). For remote clusters use the Helm chart at [`deploy/helm/agentium-os/`](../helm/agentium-os/).

Some e2e diagnostics may still reference these files historically; the authoritative install path is Argo-managed Helm via `scripts/e2e-k8s/lib.sh`.
