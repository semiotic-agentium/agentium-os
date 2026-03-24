# baml-rt

Facade crate for the Agentium OS workspace. This crate re-exports the
workspace sub-crates behind feature flags so downstream users can depend on a
single crate and opt into specific capabilities.

**Authoring agents:** see the workspace guide [`docs/how-to-write-agents.md`](../../docs/how-to-write-agents.md).

## Responsibilities
- Feature-gated re-exports of core, tools, interceptors, QuickJS runtime, A2A,
  builder, and observability crates.
- Stable external API surface for consumers.
- Re-exports stream collection helpers (`collect_a2a_stream`,
  `collect_a2a_stream_until`) so edge adapters can choose collection semantics.

## Features
- `tools`: Tool traits and registry.
- `interceptor`: LLM/tool interception pipeline.
- `quickjs`: QuickJS runtime host and bridge.
- `a2a`: Agent-to-agent protocol support (depends on `quickjs`).
- `builder`: Agent build/packaging pipeline.
- `observability`: Spans, metrics, and tracing setup.
