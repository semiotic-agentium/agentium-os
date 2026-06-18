# CLAUDE.md

Guidance for AI agents working in this repository. Deep docs live in [`docs/README.md`](docs/README.md).

## Build & test

```bash
just build              # release runner
just test               # CI-like nextest (workspace, all features)
just clippy && just fmt # lint + format
pre-commit run --all-files
cargo test test_name    # single test
just up                 # local k3d + Argo CD pilot stack
just verify-k8s-pilot-package  # authoritative k8s package validation
just e2e-k8s            # k3d scenario harness
just publish-release vX.Y.Z   # semver git tag (see RELEASING.md)
```

Linux local release builds need `libdbus-1-dev`, `libcap-ng-dev`, `pkg-config`, and `typescript@6` on PATH — run `just check-host`.

### Secrets

API keys: `fnox.toml` in project root (via `FnoxFileSecretResolver`). LLM test model: `BAML_TEST_MODEL` (default `x-ai/grok-4.3`).

## Documentation map

| Role | Doc |
|------|-----|
| **Doc index** | [`docs/README.md`](docs/README.md) |
| Agent author | [`docs/assertions/how-to-write-agents.md`](docs/assertions/how-to-write-agents.md) |
| Runtime thesis / invariants | [`docs/assertions/agentium-runtime-thesis.md`](docs/assertions/agentium-runtime-thesis.md) |
| Conversation spec | [`docs/assertions/baml-rt-conversation-spec.md`](docs/assertions/baml-rt-conversation-spec.md) |
| Rust review canon | [`docs/assertions/production-rust.md`](docs/assertions/production-rust.md) |
| Testing philosophy | [`docs/assertions/testing-handbook.md`](docs/assertions/testing-handbook.md) |
| Runner / HTTP API | [`docs/reference/agent-runner.md`](docs/reference/agent-runner.md) |
| CLI (`cargo-agent-platform`) | [`docs/reference/sdk-cli.md`](docs/reference/sdk-cli.md) |
| Host tools / MCP | [`docs/reference/host-tool-guide.md`](docs/reference/host-tool-guide.md), [`docs/reference/agentium-mcp-support.md`](docs/reference/agentium-mcp-support.md) |
| K8s operator | [`docs/runbooks/k8s-pilot-operator-guide.md`](docs/runbooks/k8s-pilot-operator-guide.md) |
| OTel metrics | [`docs/reference/metrics-inventory.md`](docs/reference/metrics-inventory.md) |
| Architecture overview | [`README.md`](README.md) |
| Contributor norms | [`AGENTS.md`](AGENTS.md) |

## Crate map (one line each)

| Crate | Role |
|-------|------|
| baml-rt-core | Errors, event bus, step executor outcomes |
| baml-rt-tools | Tool trait, session FSM, MCP config |
| baml-rt-quickjs | QuickJS bridge, BAML execution |
| baml-rt-a2a | A2A JSON-RPC, SSE, task lifecycle |
| baml-rt-conversation | Pure conversation projection |
| baml-rt-provenance | Graph persistence (SurrealDB) |
| baml-rt-repository | Content-addressable agent packages |
| baml-rt-api | HTTP API surface |
| baml-rt-builder | Agent build pipeline |
| baml-agent-runner | A2A host binary |
| cargo-agent-platform | CLI for scaffold/publish/deploy/chat |

Full crate tree and runtime flows: [`README.md`](README.md).

## Key conventions (pointers only)

- **Host tools:** session FSM in Rust; JS never mediates execution — [`docs/assertions/how-to-write-agents.md`](docs/assertions/how-to-write-agents.md) §3
- **Graph-first reads:** traverse edges, never parse node ID prefixes — [`docs/assertions/agentium-runtime-thesis.md`](docs/assertions/agentium-runtime-thesis.md)
- **Best conversation example:** `tests/fixtures/agents/task-lifecycle-demo/src/index.ts`
- **Regen fixtures:** `just regen-fixtures` after BAML/tool contract changes
- **CI:** single nextest workspace job; doc-only changes skip Rust CI (`docs/**`)
- **Release builds:** `cargo build --profile release-dist` for prebuilt binary reproduction
