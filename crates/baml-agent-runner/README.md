# baml-agent-runner

CLI for loading and executing packaged agents.

**Authoring agents:** [`docs/how-to-write-agents.md`](../../docs/how-to-write-agents.md) — manifests, BAML, and how turns surface to operators (`StructuredReply`, citations).

## Responsibilities

- Load and validate packaged agent archives (tar.gz with manifest, dist, baml_src).
- Initialize QuickJS runtime, inject the A2A shim, and register BAML functions.
- Handle A2A requests (stdio or HTTP when built with `baml-rt-api`); invoke
  `onChatMessage(message)` so agent logic runs via the DSL (`run(ctx)` or
  `session(message).run(...)`).
- Uses stream-first A2A handling internally; stdio/HTTP adapters choose whether
  to collect stream responses or forward them as live events.

The **task-lifecycle-demo** fixture (`tests/fixtures/agents/task-lifecycle-demo/`) is the in-repo reference for `awaitInput` and multi-turn lifecycle; the how-to doc links it with other patterns.

## Provenance architecture

- In SurrealDB mode, the runner wires a single concrete provenance store and
  projects it into narrow trait interfaces used by A2A/runtime components.
- Task/message/status/artifact writes and provenance events share the same
  underlying store instance (single causality graph).
- When HTTP serving is enabled, Mermaid endpoints can render graph-backed
  sequence diagrams for context/task provenance views.
