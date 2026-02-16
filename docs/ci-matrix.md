# CI Matrix

This project uses a split CI matrix to keep memory usage and compile times under control.

## Jobs

1. **Cargo test (light)**
   - Purpose: fast feedback for core crates.
   - Command:
     - `cargo test -p baml-rt-core -p baml-rt-id -p baml-rt-observability`
     - `cargo test -p baml-derive-tests`
   - Features: default only.

2. **Cargo test (heavy, serial)**
   - Purpose: run the rest of the workspace (serial) with HTTP tool features enabled.
   - Command:
     - `cargo test --workspace --exclude baml-rt-core --exclude baml-rt-id --exclude baml-rt-observability --exclude baml-derive-core --exclude baml-derive --exclude baml-derive-tests --exclude baml-rt-provenance --exclude baml-rt-a2a --exclude baml-agent-runner --features baml-rt-tools/http-tools,baml-rt-builder/http-tools -j 1`
   - LLM tests run live and require `OPENROUTER_API_KEY` (or can be skipped with `BAML_SKIP_LLM_TESTS`).

3. **FalkorDB-gated tests**
   - Purpose: run tests that require Docker + FalkorDB.
   - Command:
     - `cargo test -p baml-rt-provenance -p baml-rt-a2a -p baml-agent-runner --features baml-rt-provenance/falkordb-tests,baml-rt-a2a/falkordb-tests,baml-agent-runner/falkordb-tests,baml-agent-runner/http-tools -j 1`
   - LLM tests run live and require `OPENROUTER_API_KEY` (or can be skipped with `BAML_SKIP_LLM_TESTS`).

## Feature Flags

- `baml-rt-tools/http-tools`: enables Notion + ClickUp (reqwest + notion-client).
- `baml-agent-runner/http-tools`: compiles runner with HTTP tools.
- `baml-rt-builder/http-tools`: compiles builder with HTTP tools.
- `falkordb-tests`: enables tests that require FalkorDB + testcontainers.
LLM tests run live against the configured provider. Use `BAML_SKIP_LLM_TESTS=1` to skip them locally.

## Notes

- The default workspace build avoids heavy HTTP or Docker dependencies.
- FalkorDB tests should only run in CI where Docker is available.
