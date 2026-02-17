# baml-agent-runner

CLI for loading and executing packaged agents.

## Responsibilities

- Load and validate packaged agent archives (tar.gz with manifest, dist, baml_src).
- Initialize QuickJS runtime, inject the A2A shim, and register BAML functions.
- Handle A2A requests (stdio or HTTP when built with `baml-rt-api`); invoke
  `onChatMessage(message)` so agent logic runs via the DSL (`run(ctx)` or
  `session(message).run(...)`).
- Uses stream-first A2A handling internally; stdio/HTTP adapters choose whether
  to collect stream responses or forward them as live events.

Agents are written against the A2A DSL (see **task-lifecycle-demo** in
`tests/fixtures/agents/task-lifecycle-demo/` for the reference conversation
handling example).
