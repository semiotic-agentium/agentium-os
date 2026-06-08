# Argo CD (local k3d)

| Path | Role |
|------|------|
| [`track.json`](track.json) | `repoURL`, `targetRevision`, `imageTag` |
| [`apps/`](apps/) | Application templates (`__AGENTIUM_*` placeholders) |
| [`.rendered/`](.rendered/) | Rendered manifests — apply these only |

## Local flow

```bash
just image-tag-nonce    # optional; just up generates a nonce
just up               # k3d + registry push + Argo sync
just sync             # rebuild image + re-sync after code changes
```

Generated image tags live in [`deploy/values/generated/images.yaml`](../values/generated/images.yaml) (from `render-values.sh`).

## Render + apply

```bash
bash scripts/e2e-k8s/render-argocd-apps.sh
kubectl apply -f deploy/argocd/.rendered/
```

Override Git source for forks: `AGENTIUM_GIT_REPO`, `AGENTIUM_GIT_REVISION`.

## Semver

- Git tag: `vX.Y.Z` must match `Cargo.toml` workspace version (`just publish-release vX.Y.Z`)
- Container tag for releases: `vX.Y.Z` (same as git tag)
- Do not point deploy refs at a semver tag before images exist — see [`RELEASING.md`](../../RELEASING.md)
