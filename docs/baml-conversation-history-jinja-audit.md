# Audit: BAML Jinja and `ctx.tags['conversation_history']`

Normative spec: [baml-rt-conversation-spec.md](baml-rt-conversation-spec.md) (each row: `role`, `content`; message rows may add optional `citations`).

Canonical Jinja (loop variable `message`, two lines, `_.role` for display): constant `BAML_CONVERSATION_HISTORY_JINJA_BLOCK` in [crates/baml-rt-builder/src/builder/baml_gen/prompt_copy.rs](../crates/baml-rt-builder/src/builder/baml_gen/prompt_copy.rs).

| Path | `for` var | Row layout | Notes |
|------|-----------|------------|--------|
| `agents/slack-agent/baml_src/slack_prompt.baml` | `message` | `_.role` + `content` (two lines) | Canonical. |
| `agents/clickup-agent/baml_src/clickup_prompt.baml` | `message` | `_.role` + `content` | Canonical. |
| `agents/notion-agent/baml_src/notion_prompt.baml` | `message` | `_.role` + `content` in first fn; `_.role` + `content` in FSM/result fns | Canonical. |
| `agents/extrospection-agent/baml_src/extrospection_prompt.baml` | `message` | `_.role` + `content` (session blocks); no one-line `role: content` | Canonical. |
| `agents/claude-session-demo/baml_src/claude_session_demo_prompt.baml` | `message` | `_.role` + `content` | Canonical. |
| `tests/fixtures/agents/conversational-persona-demo/baml_src/persona_prompt.baml` | `message` | `_.role` + `content` | Reference style. |
| `tests/fixtures/agents/frontend-expert/baml_src/frontend.baml` | `message` | `_.role` + `content` | |
| `tests/fixtures/agents/tool-discovery-demo/baml_src/discovery_prompt.baml` | `message` | `_.role` + `content` | |
| `tests/fixtures/agents/argument-cleese/baml_src/argument.baml` | `message` | `_.role` + `content` | |
| `tests/fixtures/agents/coordinator-smoke/baml_src/planner.baml` | `message` | `_.role` + `content` | |
| `tests/fixtures/agents/coordinator-smoke/baml_src/coordinator_agent_prompt.baml` | `message` | `_.role` + `content` | |
| `tests/fixtures/agents/session-tool-eval/baml_src/session_eval_prompt.baml` | `message` | `_.role` + `content` | |
| `tests/fixtures/agents/security-eval-agent/baml_src/security_eval.baml` | `message` | `PresentReportingToUser`: `{{ loop.index }}.` then `_.role` + `content` (three lines/row) | `ExecuteReportingStep` uses two-line `_.role` + `content` only. |
| `tests/fixtures/agents/stream-baml-tool/baml_src/calc_prompt.baml` | `message` | `_.role` + `content` | |
| `tests/fixtures/agents/conversational-context-auto/baml_src/calc_prompt.baml` | `message` | Labeled `role: …` / `content: …` (demo/debug) | Intentional alternate layout. |
| `**/_baml_runtime.baml` under `agents/` and `tests/fixtures/agents/` | (generated) | Aligned with above via `regen_fixtures` | Regenerate; do not hand-edit for history blocks. |

**Wire fields in templates:** In history loops, only access `.role`, `.content`, and (if you render it) optional `.citations` — with `{% if message.citations %}` for optional presence. The repo script [../scripts/check-baml-conversation-history.sh](../scripts/check-baml-conversation-history.sh) enforces the allowlist; optional: warn on discouraged one-line `{{ … .role }}: {{ … .content }}` (see script env vars).

**Out of scope:** BAML that passes history as a plain `string` parameter (e.g. coordinator `conversation_context`) is not a `ctx.tags` row list and is not covered by the same Jinja check.

Last reviewed: 2026-04 (aligned with [how-to-write-agents.md §6](how-to-write-agents.md#63-jinja-conversation_history-rows) implementation).
