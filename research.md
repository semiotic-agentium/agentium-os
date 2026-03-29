The research brief has been written to `research.md`. Here's a summary of what was covered and the key actionable findings:

---

## Key Findings

### Critical Gaps in the Current `chat.rs` Implementation

After reading the actual `send_message_sse` function, I found 10 concrete issues:

**High severity (fix immediately, no deps needed):**
1. **No idle timeout** — `resp.chunk().await` hangs forever on a stalled stream. Fix: `tokio::time::timeout(45s, ...)` around each chunk poll — tokio is already in the workspace.
2. **No connect timeout** — hangs on unreachable servers. Fix: `reqwest::Client::builder().connect_timeout(Duration::from_secs(10))`.
3. **No progressive rendering** — user sees nothing until the full response arrives. Fix: `print!()` + `io::stdout().flush()?` per chunk.

**Medium severity (switch to `reqwest-eventsource`):**
4. **Multi-line `data:` not accumulated** — the spec requires concatenating multiple `data:` lines with `\n`; current code overwrites `latest_text` on each line.
5. **`event:` field ignored** — can't dispatch by event type without parsing JSON body first.
6. **`id:` field ignored** — no `Last-Event-ID` on reconnect; violates spec.
7. **No reconnection at all** — a transient TCP reset returns an error to the user instead of retrying.

**Low severity:**
8. `retry:` field ignored (server backoff hints discarded)
9. `\r\n` line endings not handled
10. Error classification has no `is_retryable()` distinction

### Recommended Crate
**`reqwest-eventsource`** — reuses the existing pinned `reqwest = "=0.12.26"` client, no extra HTTP stack, exposes `msg.event`, `msg.data` (multi-line joined), `msg.id`, `msg.retry` per spec.