# Session Handoff

Updated after the hardening pass that followed coordinator/concurrency integration.

## Metadata

- Date: 2026-03-04
- Current branch: `feat/task-daemon-demo-slice`
- Head commit: `66eb776` (`fix(task-daemon): harden prompt injection defenses, session routing, and delivery guarantees`)
- Recent commits in this slice:
  - `66eb776` fix(task-daemon): harden prompt injection defenses, session routing, and delivery guarantees
  - `26e9a04` fix: prevent message loss, prompt injection, and stale sessions in task-daemon pipeline
  - `bca6d02` docs(demo): make task-daemon run repeatable for leadership demo
- Branch ancestry:
  - rebased onto `origin/feat/concurrent-agent-calls` (`75e523e`)
  - ahead of that branch by 6 commits (`0 6`)
  - ahead of `origin/main` by 12 commits (`0 12`)

## Mission Context

- Poll Slack project-channel discussions.
- Interpret discussion meaning with LLM-first extraction.
- Hand off structured high-agency context to coordinator.
- Preserve provenance visibility and live stream correctness for demo reliability.

## Progress Since Last Handoff

### 1) A2A live-stream/session correctness

- Fixed resume session lookup mismatch where first-turn sessions (context-only key) could not be found by task-scoped resume turns.
  - Resume now falls back to context-scoped session key when appropriate.
- Fixed `session_task_id_str` propagation so first-turn live streams derive task id from resolved request scope instead of fabricating `stream-{context}` ids.
- Cleared `relay_tx` on error breaks to reduce stale-session race windows.

Files:
- [crates/baml-rt-a2a/src/a2a_transport.rs](/Users/joseph/git/semiotic-agentium/agent-platform/crates/baml-rt-a2a/src/a2a_transport.rs)

### 2) Task-daemon delivery and prompt safety

- Switched daemon state persistence to post-delivery commit:
  - Old behavior: persisted cursor before sink delivery (at-most-once, possible loss on sink failure).
  - New behavior: commit after successful delivery (best-effort at-least-once for interpreted poll windows).
- Added explicit untrusted-data fences in coordinator prompt rendering for LLM-derived interpretation/task text.
- Sanitized `source_label` before using it in trusted prompt instructions.
- Tightened `response_format_unsupported` detection to avoid broad retries on unrelated 400s.

Files:
- [crates/task-daemon/src/daemon.rs](/Users/joseph/git/semiotic-agentium/agent-platform/crates/task-daemon/src/daemon.rs)
- [crates/task-daemon/src/sink.rs](/Users/joseph/git/semiotic-agentium/agent-platform/crates/task-daemon/src/sink.rs)
- [crates/task-daemon/src/llm_extract.rs](/Users/joseph/git/semiotic-agentium/agent-platform/crates/task-daemon/src/llm_extract.rs)

### 3) Coordinator foreach and handoff hardening

- Foreach children no longer inherit upstream deps after expansion (`depends_on: []`).
- `validateWorkflowPlan` now enforces that `foreach_from` references an existing source node and that source appears in `depends_on`.
- Added untrusted-data boundaries around foreach item context injected into prompts.
- Added per-field truncation in structured handoff prompt construction to bound untrusted payload growth.
- Added explicit note that `PlanCoordinatorWorkflowBestEffort` is intentionally retained for eval/experiments.

Files:
- [agents/coordinator-agent/src/index.ts](/Users/joseph/git/semiotic-agentium/agent-platform/agents/coordinator-agent/src/index.ts)
- [agents/coordinator-agent/baml_src/planner.baml](/Users/joseph/git/semiotic-agentium/agent-platform/agents/coordinator-agent/baml_src/planner.baml)

### 4) System tool/session follow-through

- Removed dead branch in `extract_chunk_value`.
- Fixed `system/internal_a2a` resume behavior for InputRequired multi-turn by clearing stale output receiver state and allowing subsequent send.

Files:
- [crates/tools/system/src/a2a_session.rs](/Users/joseph/git/semiotic-agentium/agent-platform/crates/tools/system/src/a2a_session.rs)

### 5) Test-path hardening

- Updated runner lifecycle E2E test to resume with explicit `task_id` instead of context-only turns.

Files:
- [crates/baml-agent-runner/tests/runner_test.rs](/Users/joseph/git/semiotic-agentium/agent-platform/crates/baml-agent-runner/tests/runner_test.rs)

## Validation Run (This Session)

- `cargo test -p baml-task-daemon` passed.
- `cargo test -p baml-rt-a2a` passed.
- `cargo test -p baml-tools-system` passed.
- `cargo test -p baml-agent-runner --test runner_test test_e2e_task_lifecycle_demo -- --nocapture` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.

## Known Gaps / Risks

- Coordinator TypeScript typecheck was not run in this environment:
  - `npx tsc -p agents/coordinator-agent/tsconfig.json --noEmit` failed due restricted npm network access (`ENOTFOUND`) and no local `./node_modules/.bin/tsc`.
- Delivery semantics are now safer for loss but remain best-effort:
  - On partial multi-sink delivery failures, retries can duplicate already-delivered side effects.
  - No dead-letter queue/retry ledger yet.

## Demo Readiness Snapshot

Must-have path status:
- Slack polling: ready.
- LLM interpretation: ready (LLM-first mode default in current slice).
- Coordinator handoff: ready with typed handoff + untrusted-data boundaries.
- Provenance view: available via context/task propagation and stream fixes.

Reference assets:
- [docs/task-daemon-demo.md](/Users/joseph/git/semiotic-agentium/agent-platform/docs/task-daemon-demo.md)
- [scripts/run-task-daemon-demo.sh](/Users/joseph/git/semiotic-agentium/agent-platform/scripts/run-task-daemon-demo.sh)

## Next Session Start Prompt

```text
Session handoff:
- Branch: feat/task-daemon-demo-slice, head 66eb776, rebased on feat/concurrent-agent-calls.
- Critical hardening completed: live-stream resume key fallback, resolved-scope task-id propagation, post-delivery cursor commit, untrusted prompt fencing, foreach dependency/expansion fixes, internal_a2a InputRequired resume fix.
- Validation completed: task-daemon + a2a + tools/system tests and full clippy -D warnings all pass.
- Remaining practical gap: coordinator TS typecheck could not run here due missing local tsc and restricted npm network.

Immediate goal:
- Run full Slack -> task-daemon -> coordinator -> provenance demo rehearsal with the real workspace/channel config and capture fallback actions for any API/auth hiccups.
```
