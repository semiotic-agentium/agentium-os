set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set dotenv-load
provenance_db := "provenance.db"
runner_http_bind := "127.0.0.1:8080"

# Rebuilds clickup-agent package and runs it via a2a stdio.
clickup-agent:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- clickup-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as clickup-agent, but persists provenance to provenance.db for graph_exporter.
# Also starts the HTTP API on runner_http_bind for mermaid/metrics endpoints.
clickup-agent-provenance:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/clickup-agent --output clickup-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- clickup-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

# Rebuilds notion-agent package and runs it via a2a stdio.
notion-agent:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- notion-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as notion-agent, but persists provenance to provenance.db for graph_exporter.
notion-agent-provenance:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- notion-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}} --provenance-db {{provenance_db}}

# Rebuilds coordinator + notion packages and runs coordinator-agent via a2a stdio.
coordinator-agent:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/coordinator-agent --output coordinator-agent.tar.gz
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- coordinator-agent.tar.gz notion-agent.tar.gz --a2a-stdio --serve-http {{runner_http_bind}}

# Same as coordinator-agent, but persists provenance to provenance.db for graph_exporter.
coordinator-agent-provenance:
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/coordinator-agent --output coordinator-agent.tar.gz
    cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- package --agent-dir agents/notion-agent --output notion-agent.tar.gz
    cargo run -p baml-agent-runner --features http-tools -- coordinator-agent.tar.gz notion-agent.tar.gz --a2a-stdio --provenance-db {{provenance_db}} --serve-http {{runner_http_bind}}

# Runs the HTTP Notion demo script (starts runner if needed and streams one request).
notion-demo:
    ./scripts/run-notion-demo.sh

# Stops the background runner started by notion-demo.
notion-demo-stop:
    ./scripts/stop-notion-demo.sh

# Runs coordinator + notion HTTP demo and streams one coordinated request.
coordinator-demo:
    ./scripts/run-coordinator-demo.sh

# Stops the background runner started by coordinator-demo.
coordinator-demo-stop:
    ./scripts/stop-coordinator-demo.sh

fmt:
    cargo fmt --all

ci_features := "baml-rt-builder/http-tools,baml-agent-runner/http-tools,baml-agent-runner/memory,baml-rt/llm-tests,baml-agent-runner/llm-tests"

# CI parity: run the full nextest suite (mirrors rust-ci.yml "nextest" job).
# Requires: cargo-nextest and OPENROUTER_API_KEY for LLM tests.
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
    cargo nextest run --workspace --features baml-rt-builder/http-tools,baml-agent-runner/http-tools,baml-agent-runner/memory

# Export a Mermaid sequence diagram for a given context-id.
# Usage: just provenance-mermaid ctx-1771426017780-2
provenance-mermaid context_id:
    cargo run -p baml-rt-provenance --features cli --bin graph_exporter -- --db {{provenance_db}} --context-id {{context_id}} --simplify --format mermaid
