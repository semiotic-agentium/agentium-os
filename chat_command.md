# Chat Command SSE Fix Plan (Revised)

## Problem

`cargo agent-platform chat` currently prints `(no textual response)` despite a working runner/agent.

### Confirmed root cause

- The command posts to `/agents/{agent}/{instance}/a2a` and expects one JSON response.
- The runner exposes streaming A2A via `POST /agents/{agent}/{instance}/a2a/sse` (`text/event-stream`).
- Stream payloads are JSON-RPC envelopes where message/status data is typically inside `result.chunk`.

## Goals

1. Make chat work reliably with `/a2a/sse`.
2. Extract assistant text from real stream shape (`result.chunk...`).
3. Stop correctly on terminal signals.
4. Show clear, tagged errors.
5. Print end-to-end response time per user turn.

## Implementation Plan

### 1. Dependencies

**File:** `crates/cargo-agent-platform/Cargo.toml`

Notes:

- Do **not** require `reqwest-eventsource` for initial fix.
- No additional dependency is required for SSE parsing in this implementation.
- Use `reqwest` response byte chunks + line parsing (`data:`) for deterministic one-request/one-stream behavior on POST.

### 2. Replace single-response flow with SSE collection

**File:** `crates/cargo-agent-platform/src/commands/chat.rs`

- Replace `send_message(...) -> Result<Value>` with `send_message_sse(...) -> Result<ChatTurnResult>`.
- New endpoint:

```text
/agents/{agent}/{instance}/a2a/sse
```

- Keep JSON-RPC method `message.sendStream`.

#### 2.1 Add result type for response + timing

```rust
struct ChatTurnResult {
    text: String,
    elapsed_ms: u128,
}
```

### 3. Parse SSE safely (manual parser)

Read `resp.chunk().await?`, buffer text, split by newline, and process only lines beginning with `data:`.

Behavior:

- Ignore keep-alive comments/empty lines.
- Parse each `data:` payload as `serde_json::Value`.
- Continue until terminal state or stream end.

### 4. Normalize stream envelope before extraction

Add helper:

```rust
fn extract_chunk_or_result(v: &Value) -> Option<&Value> {
    let result = v.get("result")?;
    result.get("chunk").or(Some(result))
}
```

All text/state extraction should inspect this normalized value.

### 5. Text extraction rules

Add helper:

```rust
fn extract_text_from_chunk(chunk: &Value) -> Option<String>
```

Extraction order:

1. `chunk.message.parts[].text`
2. `chunk.task.history[]` latest agent-like role (`ROLE_AGENT`, `agent`, `assistant`) and `parts[].text`

Aggregation strategy:

- Track the latest non-empty assistant content (`latest_text`).
- Do not overwrite with empty text.
- Optional: if stream is known incremental for your agents, append mode can be enabled later.

### 6. Terminal detection (robust)

Add helper:

```rust
fn is_terminal(v: &Value) -> bool
```

Terminal if **any** of:

1. `result.final == true`
2. `chunk.task.status.state in { TASK_STATE_COMPLETED, TASK_STATE_FAILED, TASK_STATE_CANCELED, TASK_STATE_REJECTED }`
3. `chunk.statusUpdate.status.state in same terminal set`

### 7. Tagged error visualization

Add helper:

```rust
fn classify_error_tag(err: &anyhow::Error) -> &'static str
```

Proposed short tags:

- `NET` network/connectivity/request send/read failure
- `HTTP` non-2xx HTTP status
- `SSE` stream framing/termination problems
- `JSON` JSON parse errors in `data:` payload
- `RPC` JSON-RPC error object from stream (`error.message`)
- `A2A` terminal failed/canceled/rejected task state

Display format in CLI:

```text
[ERR:<TAG>] <human-readable message>
```

Verbose mode should include compact raw event/error context for diagnosis.

### 8. End-of-turn elapsed time output

Measure `Instant::now()` right before sending each request and stop when terminal/stream end is reached.

Add footer after each agent turn:

```text
[time] <N>ms
```

This is total user-message -> full agent response latency for that turn.

### 9. Update `run` output contract

For each prompt:

- Print `agent>` response text (or `(no textual response)` when truly empty)
- Then print `[time] ...ms`
- On error, print `[ERR:<TAG>] ...`

Example:

```text
you> summarize this
agent> Here is the summary...
[time] 842ms
```

Error example:

```text
agent> [ERR:RPC] tool execution failed: missing oauth token
[time] 219ms
```

### 10. Remove obsolete logic

Delete old:

- `send_message(...)`
- `extract_response_text(...)` tied to non-stream shape

## Validation Plan

### Build

```bash
cargo build -p cargo-agent-platform
```

### Manual verification

```bash
cargo agent-platform chat --agent clickup-agent --instance default --verbose
```

Check:

1. No call to `/a2a` path (should use `/a2a/sse`).
2. Assistant text appears for normal prompts.
3. Terminal states stop the loop correctly.
4. Errors show `[ERR:<TAG>]`.
5. Every turn prints `[time] <N>ms`.

### Suggested unit tests (chat.rs helpers)

1. `extract_chunk_or_result` handles both `result.chunk` and direct `result`.
2. `extract_text_from_chunk` parses `message.parts[].text`.
3. `is_terminal` true for `result.final` and terminal task/statusUpdate states.
4. JSON-RPC `error.message` becomes `RPC` tagged error.

## Future Enhancements

1. Incremental token printing (stream text as it arrives).
2. Distinct UX for `TASK_STATE_INPUT_REQUIRED` to prompt the user explicitly.
3. Artifact-aware rendering for non-text parts.
4. Retry/backoff strategy for transient `NET` failures.
