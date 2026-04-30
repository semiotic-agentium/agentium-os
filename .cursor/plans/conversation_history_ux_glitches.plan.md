---
name: Conversation history UX glitches
overview: "User report: full cat-n archive dump in history (expect ~20 lines + paging/grep), execution_session_step citations noise, duplicate user line. This plan ties symptoms to code and lists fix targets."
todos:
  - id: cap-send-done-history
    content: "Align SendDone/PageRead inline limits for UI vs BAML (PageLimit default 200 → policy e.g. 20–40 for history display paths)"
    status: pending
  - id: suppress-exec-step-ch
    content: "Apply execution_session_step empty-citations suppression (or omit tool_result rows) on conversation-history DTO path, not only Episode transcript"
    status: pending
  - id: dedupe-user-message
    content: "Reproduce duplicate user line; trace graph Message rows vs hydrate (stream + history)"
    status: pending
isProject: false
---

# Conversation history: full dump, citations noise, duplicate user

## Symptom 1 — `cat -n` dumps the whole tool response

**Cause (intentional today):** `PageLimit::DEFAULT` is **200** lines ([`crates/baml-rt-tools/src/archive_read/types.rs`](crates/baml-rt-tools/src/archive_read/types.rs) — `PageLimit::DEFAULT = 200`, comment: large enough to avoid forcing pagination).

**SendDone archive body** uses that limit in:

- [`ProjectionRenderOptions::default()`](crates/baml-rt-tools/src/prompt_projection.rs) — `send_done: PageLimit::default()` (200).
- [`episode_session_history_projection_options()`](crates/baml-rt-tools/src/prompt_projection.rs) — same `send_done: PageLimit::default()` (episode/session-history mirroring).

So a 156-line YAML listing can appear **inline in one read** — not a bug in `cat` itself, but **policy**: UI/history uses the same wide cap as “replay”, not a tight “teaser + force Read/grep”.

**Contrast:** `tool_result` in **prompt** projection defaults to `DEFAULT_TOOL_RESULT_INLINE_LINES` (**40** in the same crate — see `types.rs` `DEFAULT_TOOL_RESULT_INLINE_LINES`), which is closer to “first screenful” behavior for generic tool results, but **SendDone** is a separate cap (200).

**Fix direction (when executing):**

- Introduce a **UI/history-specific** `ProjectionRenderOptions` (or separate `send_done` cap) for conversation-history and/or episode `session_history` — e.g. align SendDone with `DEFAULT_TOOL_RESULT_INLINE_LINES` or a new constant (user asked ~20; product may choose 20 vs 40).
- Optionally strengthen builder/prompt copy (discover_agents already stresses Read/grep) so the **model** prefers paging — host caps alone do not replace LLM discipline.

---

## Symptom 2 — `a2a/execution_session_step` with `citations: []` in history

**Cause:** [`suppress_empty_execution_session_step_payload`](crates/baml-rt-provenance/src/episode/reader.rs) runs on **Episode** `prior_context` / `transcript` **after** assembly — it clears empty citation payloads for **rendered episode** / seq invariants.

The **HTTP conversation history** path ([`ConversationHistoryServiceImpl::page`](crates/baml-agent-runner/src/services/conversation_history.rs)) reads `query_conversation_context` → maps to DTOs. **Compact** profile ([`profile_filter`](crates/baml-rt-api/src/conversation_history.rs)) only runs `compact_json` on **ToolCall/ToolResult JSON** — it does **not** apply the same “strip empty citations” rule as `suppress_empty_execution_session_step_payload`, and **session_step** text blobs are not truncated by Compact the same way.

So empty `citations: []` can still appear in history rows for synthetic plan-step tools.

**Fix direction:** Mirror suppression or **omit/redact** synthetic execution_session_step tool results in conversation-history **DTO** building (or extend `profile_filter` for Compact to drop noise fields on known tool names).

---

## Symptom 3 — `user: hi` duplicated (once before tool call, again at end)

**Likely causes to verify before coding:**

1. **Two graph `Message` nodes** with the same text (e.g. resume echo, or host emitting user message twice).
2. **UI layering:** local stream user bubble + `applyConversationHistoryPage` rebuild both containing the same turn (less likely if replace is clean; more likely if **delta** appends a second user row).
3. **Ordering / prior context:** export mixes **prior** and **task** messages and labels both as `user:` with same text (fixture-specific).

**Reproduce:** Inspect `GET /contexts/{id}/conversation-history` items: count `role === user` + `content.message.text === hi` and compare `activityAnchor` / `timestampMs`.

**Fix direction:** Dedupe by activity anchor in hydration, or fix writer so only one Message node exists per user send.

---

## Relationship to [chat duplicate reply plan]

The earlier “duplicate reply” analysis focused on **multiple text blocks** from streaming + `pushTextBlock`. This report adds **policy limits** (200-line SendDone), **execution_session_step** in API history, and **duplicate user** lines — overlapping but distinct failure modes.
