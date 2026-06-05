# Contributing to Agentium OS

Thanks for your interest in contributing! This guide covers how to build, test,
lint, and submit changes. By participating you agree to abide by our
[Code of Conduct](CODE_OF_CONDUCT.md).

## Prerequisites

Agentium OS is a Rust workspace (edition 2024) with the toolchain pinned via
`rust-toolchain.toml`, so `rustup` selects the right version automatically.

Some build and test paths link the runner binary and exercise the agent build
pipeline, which need system-level dependencies and TypeScript on `PATH`:

```bash
# Linux host dependencies (installed in the runner image; absent on a fresh host)
sudo apt install -y libdbus-1-dev libcap-ng-dev pkg-config

# TypeScript 6.x is required for the agent build pipeline
npm install -g typescript@6
```

- `libdbus-1-dev` — pulled in via the secret-resolver chain (`fnox` → `keyring`).
- `libcap-ng-dev` — pulled in via `microsandbox` for the runner sandbox boundary.
- `typescript@6` — the canonical `tsconfig.json` uses `moduleResolution: "bundler"`.

Run `just check-host` to verify these are present before a release build. On
non-Linux hosts the Linux-only checks are skipped.

### Secrets for tests

API keys for tests are resolved through `fnox.toml` via `FnoxFileSecretResolver`.
The file maps secret names to values with a `default` field. Create `fnox.toml`
in the project root locally. The model used by LLM tests is controlled by the
`BAML_TEST_MODEL` environment variable, which defaults to `x-ai/grok-4.3`.

## Build

```bash
cargo build              # nightly pinned via rust-toolchain.toml
cargo build --release
```

## Test

```bash
cargo test                          # runs default-members only
cargo test --workspace              # runs all crates
cargo test -- --nocapture

# Run a single test
cargo test test_name
cargo test -p baml-rt test_name     # specific crate

# Feature-gated LLM test suites (require API keys)
cargo test -p baml-rt --features llm-tests -j 1
```

For a CI-parity run, use [`cargo-nextest`](https://nexte.st/):

```bash
cargo install cargo-nextest         # once
just test                           # full workspace pass matching rust-ci.yml
just test-unit                      # tests that need no API keys or external services
just test-crate <crate>             # a single crate's tests
```

## Lint and format

Run these before committing — CI enforces them:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

## Pre-commit hooks

The repository ships a [`pre-commit`](https://pre-commit.com/) configuration
(secret scanning, file checks, `cargo fmt`, `cargo clippy`, fixture
regeneration, and BAML/Helm checks):

```bash
pip install pre-commit
pre-commit install                  # run the hooks on every commit
pre-commit run --all-files          # run them manually across the tree
```

## Continuous integration (fork PRs)

This repository is **public**. CI uses a **trusted / untrusted** split:

| Your PR is from… | What runs |
|------------------|-----------|
| A **fork** | GitHub-hosted jobs only: fmt, clippy, nextest **without** LLM tests, reuse, gitleaks. **No** API secrets, **no** self-hosted runners, **no** k8s pilot validation. |
| A **branch in this repo** (collaborator) | Full CI on self-hosted runners, including LLM tests and k8s pilot validation when paths match. |
| After **merge to `main`** | Full trusted CI on every push. |

Fork PRs may sit idle until a maintainer **approves** the workflow run (repo
setting). Full k8s validation runs on merge to `main` or via maintainer
`workflow_dispatch`.

See [`docs/runbooks/ci-security.md`](docs/runbooks/ci-security.md) for operator
settings (branch protection, runner groups, secrets).

## Commit and PR conventions

- Use [Conventional Commits](https://www.conventionalcommits.org/) for commit
  subjects and PR titles (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`,
  `chore:`).
- Aim for atomic commits — one logical change per commit.
- Keep documentation user-focused, not implementation-focused, and describe
  current behavior (no version-history notes in comments).
- Open a pull request against `main`. Fill in the
  [pull request template](.github/PULL_REQUEST_TEMPLATE.md), link any issue the
  PR resolves with a closing keyword (`Closes #N`), and make sure the build,
  tests, clippy, and `cargo fmt --check` are green.

## Licensing

Agentium OS is licensed under the **[Apache License, Version 2.0](LICENSE)**
only. There is no separate “SPDX license” — `Apache-2.0` is the standard
[SPDX](https://spdx.org/licenses/) identifier for that same legal text.

| Declaration | Meaning |
|-------------|---------|
| [`LICENSE`](LICENSE) | Full Apache 2.0 legal text |
| [`NOTICE`](NOTICE) | Copyright and attribution |
| Root [`Cargo.toml`](Cargo.toml) `[workspace.package].license` | Crate metadata (`Apache-2.0`) |
| Source `# SPDX-License-Identifier: Apache-2.0` | Per-file tag (REUSE) |
| [`REUSE.toml`](REUSE.toml) bulk annotations | Config, docs, generated trees |

Use **`Apache-2.0`** everywhere for first-party work. Do not use `MIT`, bare
`Apache`, or other identifiers unless you are documenting **third-party**
upstream code (e.g. vendored models under `models/fastembed/` — see `REUSE.toml`).

### REUSE compliance

The repository follows [REUSE 3.3](https://reuse.software): every tracked file
must carry copyright and license metadata. The `reuse` CI job fails if anything
is missing.

New hand-written source needs a two-line SPDX header (comment style for the
language: `//`, `#`, `<!-- -->`):

```rust
// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0
```

Generate headers with the `reuse` CLI:

```bash
reuse annotate --copyright "Semiotic AI, Inc." --license Apache-2.0 \
  --year 2026 --merge-copyrights path/to/new_file.rs
```

Generated trees, content-hashed agent/fixture bundles, configuration, docs, and
other non-headered paths are covered in bulk via [`REUSE.toml`](REUSE.toml) —
add new paths there when needed. Verify locally:

```bash
reuse lint
```

## Where to look next

- `README.md` — workspace architecture and runtime flow.
- `CLAUDE.md` — pointer hub for build commands and doc map entry.
- `docs/README.md` — documentation map (assertions, reference, runbooks).
- `docs/` — design notes, agent-authoring guides, and testing documentation.

We review pull requests as quickly as we can. Thank you for contributing!
