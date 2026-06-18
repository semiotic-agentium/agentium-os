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

## Publish prebuilt Linux binaries

The GitHub Release with prebuilt binaries is created only by pushing a SemVer tag
that matches `[workspace.package].version` in `Cargo.toml`. Merging to `main`
does not publish release assets. Manual `workflow_dispatch` runs build dry-runs
only and does not upload a GitHub Release.

From an up-to-date `main` checkout:

```bash
git checkout main
git pull
VERSION=$(bash scripts/release/workspace-version.sh)
git tag "v${VERSION}"
git push origin "v${VERSION}"
```

Then watch GitHub Actions → **Release publish**. The workflow verifies the tag
against the workspace version, builds each Linux target natively, and creates or
updates the GitHub Release.

Expected release assets:

- `agentium-os-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `agentium-os-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`

If your local remote still points at the old repository path, update it before
pushing tags:

```bash
git remote set-url origin git@github.com:semiotic-agentium/agentium-os.git
```

## Dev deploy refs (remote GitOps)

Rolling dev on `main` + `latest`:

```bash
just publish-release              # commits track.json + deploy/values/dev/images.yaml
```

## Remote registry publish

Deferred: GHCR push workflow and post-publish deploy ref bump on `main`.
