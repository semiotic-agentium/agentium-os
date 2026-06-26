# Releasing Agentium OS

Releases are driven by Release Please. One SemVer must stay consistent across:

- `Cargo.toml` `[workspace.package].version`
- `Cargo.lock` workspace package entries
- `deploy/helm/agentium-os/Chart.yaml` `version` and `appVersion`
- `.release-please-manifest.json`

## Normal release flow

1. Merge feature/fix PRs to `main` using Conventional Commits (`feat:`, `fix:`, etc.).
2. Release Please opens or updates a release PR.
3. Verify release PR CI is green.
4. Merge release PR.
5. Release Please creates tag `vX.Y.Z` and a GitHub Release.
6. `Release publish` builds Linux binaries and uploads assets to that release.

Do not manually bump versions or create release tags during normal releases.

Expected release assets:

- `agentium-os-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `agentium-os-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`

## Manual fallback

Use only if Release Please is unavailable.

```bash
git checkout main
git pull
just bump-version patch   # or minor | major
git add Cargo.toml Cargo.lock deploy/helm/agentium-os/Chart.yaml
git commit -m "release: v$(just workspace-version)"
git push origin main

VERSION=$(bash scripts/release/workspace-version.sh)
git tag "v${VERSION}"
git push origin "v${VERSION}"
```

The tag must point at the commit containing all version and lockfile updates.

## Local k3d validation

```bash
just up
just verify-k8s-pilot-package
```

Image tags are nonces (`local-dev-…`) written to `deploy/values/generated/.last-image-tag`.

## Dev deploy refs (remote GitOps)

Rolling dev on `main` + `latest`:

```bash
just publish-release
```

## Remote registry publish

Deferred: GHCR push workflow and post-publish deploy ref bump on `main`.
