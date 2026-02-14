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
   - Purpose: run the rest of the workspace without heavy HTTP tools.
   - Command:
     - `cargo test --workspace --exclude baml-rt-core --exclude baml-rt-id --exclude baml-rt-observability --exclude baml-derive-core --exclude baml-derive --exclude baml-derive-tests --exclude baml-rt-a2a -j 1`
   - Features: default only.

3. **HTTP tools (serial)**
   - Purpose: exercise Notion/ClickUp tools with HTTP dependencies.
   - Command:
     - `cargo test -p baml-rt-tools -p baml-agent-runner -p baml-rt-builder -j 1 --features baml-rt-tools/http-tools,baml-agent-runner/http-tools,baml-rt-builder/http-tools`

4. **FalkorDB-gated tests**
   - Purpose: run tests that require Docker + FalkorDB.
   - Command:
     - `cargo test -p baml-rt-provenance --features falkordb-tests -j 1`
     - `cargo test -p baml-rt-a2a --features falkordb-tests -j 1`
     - `cargo test -p baml-agent-runner --features falkordb-tests -j 1`

## Feature Flags

- `baml-rt-tools/http-tools`: enables Notion + ClickUp (reqwest + notion-client).
- `baml-agent-runner/http-tools`: compiles runner with HTTP tools.
- `baml-rt-builder/http-tools`: compiles builder with HTTP tools.
- `falkordb-tests`: enables tests that require FalkorDB + testcontainers.

## Notes

- The default workspace build avoids heavy HTTP or Docker dependencies.
- FalkorDB tests should only run in CI where Docker is available.
