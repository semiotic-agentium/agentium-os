# Agent Runner Notes

This document captures operational details of the HTTP runner that are easy to
miss when refactoring. It is intended for coding agents and maintainers.

## HTTP A2A Endpoints

- `POST /agents/{agent}/default/a2a`  
  Collects the full A2A stream and returns JSON-RPC responses.

- `POST /agents/{agent}/default/a2a/sse`  
  Streams JSON-RPC responses over Server-Sent Events (SSE).

## SSE Stream Lifetime (Important)

The SSE handler returns a `BusStream` that is backed by tasks spawned inside the
A2A stream handler. Those tasks **must live on the same long-lived Tokio
runtime** that services HTTP requests.

### Anti-pattern

Spawning a *short-lived* runtime (e.g. `tokio::runtime::Builder::new_current_thread`)
inside `handle_a2a_by_key` causes the stream tasks to be dropped as soon as the
function returns. The client then sees only keep-alive `:` events and never
receives data.

### Correct pattern

Run `handle_a2a_stream` directly on the existing runtime:

- Keep the stream handler on the host runtime
- Use `context::with_scope` for attribution
- Avoid nested runtimes for SSE code paths

### Symptom → Fix

| Symptom | Likely Cause | Fix |
|---|---|---|
| SSE returns only keep-alives | stream tasks dropped when short-lived runtime exits | run stream handler on host runtime |

