# Documentation map

Agentium OS docs are split by purpose. **Assertions** are normative rules for agents and
reviewers. **Reference** describes current behavior. **Runbooks** cover install, test, demo, and
operations.

Assertions were validated against the codebase as of **2026-06-03**. When changing
subsystems, capture invariants in the relevant assertion doc (see
[`testing-handbook.md`](assertions/testing-handbook.md)) and update it in the same PR.

## Assertions (`docs/assertions/`)

Rules that must hold — guide agentic workflow, codegen, and review.

| Doc | Purpose |
|-----|---------|
| [agentium-runtime-thesis.md](assertions/agentium-runtime-thesis.md) | Core runtime boundary, host-mediated effects, graph-first reads |
| [how-to-write-agents.md](assertions/how-to-write-agents.md) | Primary agent-author onboarding |
| [agent-patterns.md](assertions/agent-patterns.md) | High-leverage agent patterns (structured tool output vs UX) |
| [intent-based-planning-and-session-prompting.md](assertions/intent-based-planning-and-session-prompting.md) | Plan-anchored prompting and session template ordering |
| [baml-rt-conversation-spec.md](assertions/baml-rt-conversation-spec.md) | Normative conversation boundaries and invariants |
| [citable-history-and-checked-citations.md](assertions/citable-history-and-checked-citations.md) | Citation contract (`#N` / `@N`) |
| [baml-conversation-history-jinja-audit.md](assertions/baml-conversation-history-jinja-audit.md) | BAML injection: `conversation_transcript` only |
| [host-to-agent-event-delivery.md](assertions/host-to-agent-event-delivery.md) | Event intake, subscriptions, dispatch model |
| [drift-catalogue.md](assertions/drift-catalogue.md) | Plan-anchored drift scoring scenarios |
| [production-rust.md](assertions/production-rust.md) | Production Rust patterns and anti-patterns |
| [testing-handbook.md](assertions/testing-handbook.md) | Enforced test strategy: E2E authority, boundary snapshots, matrices; TDD shards de-prioritized |
| [tool-mvp-checklist.md](assertions/tool-mvp-checklist.md) | Pre-merge checklist for new host tools |

## Reference (`docs/reference/`)

System-as-is: APIs, contracts, and current behavior.

| Doc | Purpose |
|-----|---------|
| [agent-runner.md](reference/agent-runner.md) | Runner CLI, HTTP API, deploy, auth tiers |
| [agentium-cli.md](reference/agentium-cli.md) | `agentium` command reference (`serve`, install, eval, …) |
| [host-tool-guide.md](reference/host-tool-guide.md) | External, sandbox, and static host tools |
| [agentium-mcp-support.md](reference/agentium-mcp-support.md) | MCP config, registry, build, runtime, pooling |
| [repository-api-contract.md](reference/repository-api-contract.md) | Publish/entries/blob HTTP contract |
| [deployment-lifecycle-contract.md](reference/deployment-lifecycle-contract.md) | Deploy semantics and restore policy |
| [baml-rt-repository-design.md](reference/baml-rt-repository-design.md) | Repository and hash crate design |
| [agent-conversation-crate.md](reference/agent-conversation-crate.md) | `baml-rt-conversation` crate map |
| [metrics-inventory.md](reference/metrics-inventory.md) | Exported OTLP metric names |
| [otel-trace-instrumentation-guide.md](reference/otel-trace-instrumentation-guide.md) | OTel span instrumentation patterns |
| [otel-metrics-instrumentation-guide.md](reference/otel-metrics-instrumentation-guide.md) | OTel metrics patterns and local stack |
| [task-daemon-event-contract.md](reference/task-daemon-event-contract.md) | Wire envelope `host.source-records.v1` |
| [slack-tool.md](reference/slack-tool.md) | Slack read-only tool reference |

## Runbooks (`docs/runbooks/`)

Install, test, demo, and operate.

| Doc | Purpose |
|-----|---------|
| [k8s-pilot-operator-guide.md](runbooks/k8s-pilot-operator-guide.md) | Supported Helm install path |
| [e2e-k8s.md](runbooks/e2e-k8s.md) | K8s validation harness |
| [ci-security.md](runbooks/ci-security.md) | Public-repo CI trust model and GitHub settings |
| [k8s-pilot-load-testing.md](runbooks/k8s-pilot-load-testing.md) | Load-test harness contract |
| [task-daemon.md](runbooks/task-daemon.md) | Task-daemon user guide |
| [host-tool-quickstart.md](runbooks/host-tool-quickstart.md) | Sandboxed external tool + agent checklist |
| [coordinator-demo.md](runbooks/coordinator-demo.md) | Coordinator delegation demo |
| [notion-demo.md](runbooks/notion-demo.md) | Notion agent demo |
| [slack-it-requirements.md](runbooks/slack-it-requirements.md) | Slack Business+ IT questionnaire |

## Crate-local docs

Co-located with implementation; linked from reference or assertions as needed.

| Path | Type |
|------|------|
| [crates/baml-rt-a2a/docs/INVARIANTS_AND_LIVENESS.md](../crates/baml-rt-a2a/docs/INVARIANTS_AND_LIVENESS.md) | assertion |
| [crates/baml-rt-tools/docs/llm_json_boundary.md](../crates/baml-rt-tools/docs/llm_json_boundary.md) | assertion |
| [crates/baml-rt-api/docs/A2A_COMPLIANCE.md](../crates/baml-rt-api/docs/A2A_COMPLIANCE.md) | reference |
| [crates/baml-rt-api/docs/OTEL_COMPLIANCE.md](../crates/baml-rt-api/docs/OTEL_COMPLIANCE.md) | reference |
| [crates/baml-rt-a2a/docs/A2A_SESSION_CHANNEL_DESIGN.md](../crates/baml-rt-a2a/docs/A2A_SESSION_CHANNEL_DESIGN.md) | reference |
| [crates/baml-rt-provenance/PROV_MAPPING.md](../crates/baml-rt-provenance/PROV_MAPPING.md) | reference |

## Reading order by role

| Role | Start here |
|------|------------|
| AI agent in repo | [CLAUDE.md](../CLAUDE.md) → assertions |
| Agent author | [how-to-write-agents.md](assertions/how-to-write-agents.md) → [agent-patterns.md](assertions/agent-patterns.md) |
| Rust tool author | [host-tool-guide.md](reference/host-tool-guide.md) → [tool-mvp-checklist.md](assertions/tool-mvp-checklist.md) |
| Operator / K8s | [k8s-pilot-operator-guide.md](runbooks/k8s-pilot-operator-guide.md) → [agent-runner.md](reference/agent-runner.md) |
| Observability | [metrics-inventory.md](reference/metrics-inventory.md) → OTel guides |
| Testing | [testing-handbook.md](assertions/testing-handbook.md) → [e2e-k8s.md](runbooks/e2e-k8s.md) |
| Operator UI | [`web/README.md`](../web/README.md) → provenance + chat views |
