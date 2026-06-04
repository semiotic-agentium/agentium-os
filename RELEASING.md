# Releasing Agentium OS

One SemVer across [`Cargo.toml`](../Cargo.toml) `[workspace.package].version` and [`deploy/helm/agentium-os/Chart.yaml`](../deploy/helm/agentium-os/Chart.yaml).

## Local k3d validation

```bash
just up                    # cluster + build + Argo sync
just verify-k8s-pilot-package   # authoritative CI-like validator
```

Image tags are nonces (`local-dev-…`) written to `deploy/values/generated/.last-image-tag`.

## Cut a release

1. Bump version:
   ```bash
   just bump-version patch   # or minor | major
   git commit -am "release: v$(just workspace-version)"
   ```
2. Validate on k3d: `just verify-k8s-pilot-package`
3. Tag (tag only — do not bump deploy refs until images exist):
   ```bash
   just publish-release vX.Y.Z
   ```
4. **Do not** `just release vX.Y.Z` for semver until container images exist at that tag.

## Dev deploy refs (remote GitOps)

Rolling dev on `main` + `latest`:

```bash
just publish-release              # commits track.json + deploy/values/dev/images.yaml
```

## Remote registry publish

Deferred: GHCR push workflow and post-publish deploy ref bump on `main`.
