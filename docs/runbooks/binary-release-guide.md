# Binary release guide

Agentium OS publishes prebuilt Linux binaries for x86_64 and aarch64 via GitHub Releases. This guide covers the automated release pipeline and manual procedures.

## Release pipeline overview

The release process is fully automated via `.github/workflows/release-publish.yml`:

1. **Tag push** (`v*.*.*`): Full build + GitHub Release upload
2. **Manual dispatch**: Full build (no upload) — dry-run of the matrix
3. **PR** (release-tooling paths): aarch64 native-build smoke test only

### Build matrix

Targets are defined in `scripts/release/release-matrix.json` (the SSOT):

- **x86_64-unknown-linux-gnu**: ubuntu-22.04 runner
- **aarch64-unknown-linux-gnu**: ubuntu-22.04-arm runner

Each target builds **natively** on its own architecture — no cross-compilation. This avoids onnxruntime prebuilt compatibility issues (requires glibc ≥2.32 / GCC ≥11).

### Shipped binaries

- `baml-agent-runner` (features: http-tools, memory, sandbox-provider)
- `baml-agent-builder` (features: http-tools, memory)
- `cargo-agent-platform` (default features)

## Release artifacts

Each GitHub Release contains:

- `agentium-os-v{version}-{target}.tar.gz` per target
- `SHA256SUMS` (aggregate checksums)
- Auto-generated release notes

Tarball contents:
- Release binaries
- `SHA256SUMS` (per-tarball binary checksums)
- `INSTALL.md` (installation guide)

## Release process

Release Please owns normal releases:

1. Conventional Commits land on `main`.
2. Release Please opens or updates a release PR.
3. Release PR postprocess workflow updates `Cargo.lock` on that PR.
4. Merge the release PR after CI is green.
5. Release Please creates `vX.Y.Z` and a GitHub Release.
6. Same Release Please workflow builds binaries and uploads assets to that release.

Manual tag pushes are fallback-only. If used, the tag must point at a commit where `Cargo.toml`, `Cargo.lock`, and `deploy/helm/agentium-os/Chart.yaml` all contain the same version.

The binary publish workflow automatically:
1. Verifies tag matches `[workspace.package].version` and Helm `appVersion`
2. Builds all targets natively
3. Packs tarballs with reproducible archives
4. Uploads assets to the GitHub Release

### Dry-run testing

Test the full build matrix without publishing:

```bash
# Via GitHub UI: Actions → Release publish → Run workflow
# Or via gh CLI:
gh workflow run release-publish.yml
```

## Local reproduction

Reproducible builds via the `release-dist` Cargo profile:

```bash
# Install host dependencies (Ubuntu/Debian)
sudo apt-get install build-essential pkg-config libssl-dev \
  libdbus-1-dev libcap-ng-dev clang libclang-dev llvm-dev lld

# Build release binaries
cargo build --profile release-dist --features http-tools,memory,sandbox-provider -p baml-agent-runner
cargo build --profile release-dist --features http-tools,memory -p baml-rt-builder
cargo build --profile release-dist -p cargo-agent-platform

# Or use the release scripts
scripts/release/build-release-binaries.sh  # host target
scripts/release/pack-target-tarball.sh --target $(rustc -vV | sed -n 's/^host: //p')
```

## Profile configuration

The `release-dist` profile (defined in root `Cargo.toml`):

```toml
[profile.release-dist]
inherits = "release"
lto = "thin"          # Bounded CI memory/time
codegen-units = 16    # Parallel codegen
strip = true          # Smaller artifacts
```

This balances build time/memory with binary optimization for CI constraints.

## Troubleshooting

### Build failures

**onnxruntime linking errors**: Ensure native builds on runners with glibc ≥2.35 (ubuntu-22.04+). Cross-compilation is not supported.

**Missing dependencies**: Install the full host dependency list per `INSTALL.md`.

### Release upload failures

The workflow is idempotent — re-running after partial failure reuses the existing release and clobbers assets instead of erroring.

### Version mismatches

Tag must exactly match `[workspace.package].version` in root `Cargo.toml`. The workflow fails fast on mismatches.

## Security considerations

- Reproducible archives: stable entry order, no uid/gid/mtime leakage
- SHA256 checksums for integrity verification
- Native builds prevent supply-chain attacks via cross-compilation toolchains
- Minimal CI permissions: `contents: write` only for release job

## Future enhancements

- Code signing for binaries
- Additional target architectures (pending onnxruntime support)
- Container images alongside tarballs
- Automated security scanning of release artifacts
