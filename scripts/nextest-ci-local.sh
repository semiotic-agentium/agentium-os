#!/usr/bin/env bash
# Run nextest with the same feature set and profile as CI (for local testing).
# Requires: cargo-nextest (cargo install cargo-nextest)
# For LLM tests: set OPENROUTER_API_KEY (e.g. source .env before running, or export it).
# baml-rt, baml-rt-interceptor, and baml-rt-builder are in the llm test group (max-threads = 1)
# so LLM-dependent tests don't all hit the same backend concurrently and queue for 180–300s.
set -euo pipefail
cd "$(dirname "$0")/.."

# Propagate OPENROUTER_API_KEY to nextest (and thus to test binaries); .env is optional.
if [[ -f .env ]]; then
  set -a
  source .env
  set +a
fi

# Unified feature set: one build, one run (matches rust-ci single nextest job).
NEXTEST_FEATURES=(
  baml-rt-builder/http-tools
  baml-agent-runner/http-tools
  baml-agent-runner/memory
  baml-rt/llm-tests
  baml-agent-runner/llm-tests
)
FEATURES_STR=$(IFS=,; echo "${NEXTEST_FEATURES[*]}")

echo "Running: cargo nextest run --workspace --locked --profile ci --features ${FEATURES_STR}"
cargo nextest run --workspace --locked --profile ci --features "$FEATURES_STR"

echo ""
echo "JUnit report (if run completed): target/nextest/ci/junit.xml"
