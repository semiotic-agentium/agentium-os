# Repository Guidelines

## Project Structure & Module Organization
This repository is a Rust workspace centered on `crates/`, which holds the runtime, runner, builder, tool, integration, and support crates. Product agents live in `agents/<agent-name>/`; reusable and test-only agents live in `tests/fixtures/agents/`. The Vue UI is isolated in `web/`. Operational assets live in `observability/` and `deploy/`. Documentation lives in `docs/` — see [`docs/README.md`](docs/README.md) for the assertion / reference / runbook map.

Use `baml_src/` for shared example schemas, and `crates/test-support` for common test helpers instead of copying setup code.

## Build, Test, and Development Commands
- `just build` or `just build-debug`: build `baml-agent-runner` in release or debug mode.
- `just runner`: build and start the local runner on `127.0.0.1:18080`.
- `just web-build`: install web dependencies and build the Vite app in `web/dist`.
- `just verify-agentium-console`: Vitest + build + runner API checks (Agents load parity).
- `just verify-bootstrap-eval`: bootstrap → publish → deploy → deterministic A2A eval.
- `just fmt`: run workspace formatting.
- `just clippy`: run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- `just test`: run the full `cargo nextest` LLM and non-LLM suites with CI-like flags.
- `just test-unit` or `just test-crate baml-rt-provenance`: faster local verification.
- `just regen-fixtures`: regenerate committed agent artifacts such as `baml_src/_baml_runtime.baml` and `src/baml-runtime.d.ts`.
- `just up` / `just sync`: local k3d pilot stack via Argo CD (see [`RELEASING.md`](RELEASING.md)).
- `just verify-k8s-pilot-package`: authoritative k8s install validation (CI mirror).

## Coding Style & Naming Conventions
Rust uses edition 2024 and nightly `rustfmt`; keep formatting tool-driven, with grouped imports and a 100-column width. Use `snake_case` for Rust modules, files, and tests. TypeScript/Vue in `web/` and `agents/` follows Prettier defaults here: 2-space indentation, semicolons, double quotes, and trailing commas. Prefix intentionally unused TS variables with `_` to satisfy ESLint.

Do not hand-edit generated runtime files without immediately regenerating them.

## Testing Guidelines
Prefer authority E2E and vertical slices, then boundary `insta` snapshots and `*_matrix` tables—not TDD-style per-variant unit shards. Put crate-specific tests in `crates/*/tests/*.rs`; share helpers through `crates/test-support`; keep agent fixtures in `tests/fixtures/agents/`. Use `#[tokio::test]` for async surfaces. One authoritative E2E per behavior; see `docs/assertions/testing-handbook.md`.

## Commit & Pull Request Guidelines
Recent history follows Conventional Commit prefixes such as `feat:`, `fix:`, `test:`, and `docs:`. Keep subjects imperative and scoped to one change. PRs should describe affected crates or agents, list the commands you ran (`just test-unit`, `just clippy`, etc.), call out regenerated fixtures, and include screenshots only for `web/` UI changes. Link the issue when relevant.

## Configuration & Generated Assets
Keep secrets in `.env` or `fnox.toml`; never commit tokens. Large model assets under `models/fastembed/` are Git LFS-managed and should not be rewritten casually.
