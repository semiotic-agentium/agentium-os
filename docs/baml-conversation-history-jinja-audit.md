# BAML conversation context

**Canonical injection:** `{{ ctx.tags['conversation_transcript'] }}` only. The host does not expose `ctx.tags['conversation_history']` (JSON row array) to BAML.

Normative docs:

- [intent-based-planning-and-session-prompting.md](intent-based-planning-and-session-prompting.md) — template ordering and backend projection.
- [how-to-write-agents.md §6](how-to-write-agents.md) — ref-table `#N` / `@N` semantics as rendered into the transcript string.

Enforcement: [scripts/check-baml-conversation-history.sh](../scripts/check-baml-conversation-history.sh) fails if any agent/fixture BAML references `ctx.tags` `conversation_history`.

The former per-file loop inventory is retired; row-shaped projection remains an internal Rust/API concern (`project_prompt_context`, HTTP snapshots), not a Jinja surface.
