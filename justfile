set dotenv-load := true
set shell := ["/bin/zsh", "-lc"]

default: test-all

# Matches CI "light" job.
test-light:
    cargo test --locked -p baml-rt-core -p baml-rt-id -p baml-rt-observability

# Matches CI derive tests.
test-derive:
    cargo test --locked -p baml-derive-tests

# Matches CI "heavy" job (serial).
test-heavy:
    cargo test --locked --workspace --exclude baml-rt-core --exclude baml-rt-id --exclude baml-rt-observability --exclude baml-derive-core --exclude baml-derive --exclude baml-derive-tests --exclude baml-rt-provenance --exclude baml-rt-a2a --exclude baml-agent-runner --features baml-rt-tools/http-tools,baml-rt-builder/http-tools -j 1

# Matches CI FalkorDB-gated job (serial).
test-falkordb:
    cargo test --locked -p baml-rt-provenance -p baml-rt-a2a -p baml-agent-runner --features baml-rt-provenance/falkordb-tests,baml-rt-a2a/falkordb-tests,baml-agent-runner/falkordb-tests,baml-agent-runner/http-tools -j 1

# Cleanup the reusable FalkorDB container created by testcontainers.
falkordb-clean:
    docker rm -f baml-falkordb-tests || true

# Run the full CI suite locally (requires OPENROUTER_API_KEY and FalkorDB).
test-all: test-light test-derive test-heavy test-falkordb

# Regenerate fixture runtime declarations and snapshots intentionally.
regen:
    cargo run -p baml-rt-builder --bin regen_fixtures
