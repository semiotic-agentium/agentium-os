set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set dotenv-load
provenance_db := "provenance.db"

# Rebuilds clickup-agent package and runs it via a2a stdio.
clickup-agent:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- clickup-agent.tar.gz --a2a-stdio

# Same as clickup-agent, but persists provenance to provenance.db for graph_exporter.
clickup-agent-provenance:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- clickup-agent.tar.gz --a2a-stdio --provenance-db {{provenance_db}}

# Rebuilds notion-agent package and runs it via a2a stdio.
notion-agent:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- notion-agent.tar.gz --a2a-stdio

# Same as notion-agent, but persists provenance to provenance.db for graph_exporter.
notion-agent-provenance:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- notion-agent.tar.gz --a2a-stdio --provenance-db {{provenance_db}}

# Runs the HTTP Notion demo script (starts runner if needed and streams one request).
notion-demo:
    ./scripts/run-notion-demo.sh

# Stops the background runner started by notion-demo.
notion-demo-stop:
    ./scripts/stop-notion-demo.sh

fmt:
    cargo fmt --all

ci_features := "baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-rt-provenance/falkordb-tests,baml-rt-a2a/falkordb-tests,baml-agent-runner/falkordb-tests,baml-agent-runner/http-tools,baml-rt/llm-tests,baml-agent-runner/llm-tests"

# CI parity: run the full nextest suite (mirrors rust-ci.yml "nextest" job).
# Requires: cargo-nextest, a running FalkorDB on localhost:6379,
# and OPENROUTER_API_KEY for LLM tests.
test:
    cargo nextest run --workspace --features {{ci_features}}

# Same as `test` but only compile — useful for a quick pre-push check.
test-build:
    cargo nextest run --workspace --features {{ci_features}} --no-run

# Run only a single crate's tests (e.g. `just test-crate baml-rt-provenance`).
test-crate crate:
    cargo nextest run -p {{crate}} --features {{ci_features}}

# Run tests that don't need FalkorDB or API keys (unit tests only).
test-unit:
    cargo nextest run --workspace --features baml-rt-tools/http-tools,baml-rt-builder/http-tools

# Export a Mermaid sequence diagram for a given context-id.
# Usage: just provenance-mermaid ctx-1771426017780-2
provenance-mermaid context_id:
    cargo run -p baml-rt-provenance --features cli --bin graph_exporter -- --db {{provenance_db}} --context-id {{context_id}} --simplify --format mermaid
