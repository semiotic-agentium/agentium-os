# baml-agent-runner

CLI for loading and executing packaged agents.

## Responsibilities

- Load and validate packaged agent archives (tar.gz with manifest, dist, baml_src).
- Initialize QuickJS runtime, inject the A2A shim, and register BAML functions.
- Handle A2A requests (stdio or HTTP when built with `baml-rt-api`); invoke
  `onChatMessage(message)` so agent logic runs via the DSL (`run(ctx)` or
  `session(message).run(...)`).

Agents are written against the A2A DSL (see **task-lifecycle-demo** in
`tests/fixtures/agents/task-lifecycle-demo/` for the reference conversation
handling example).
