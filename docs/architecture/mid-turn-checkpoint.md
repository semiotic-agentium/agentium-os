# Mid-Turn Agent Checkpoint: Architecture and Feasibility

This document maps every piece of ephemeral agent state, proposes serialization strategies, defines the migration protocol state machine, and identifies hard blockers. It is written at prototype-informing depth: someone could pick this up and start implementing.

## Why this matters

Agentium OS records first-class provenance as part of execution. At turn boundaries, all agent state is either in the provenance graph (SurrealDB) or reconstructable from it. Turn-boundary migration (Phase 2 drain mechanism) exploits this: wait for the current turn to finish, move the agent code to another runtime, reconnect to the shared provenance store, and continue.

Mid-turn checkpoint goes further: capture the agent's state *during* an active turn (open tool sessions, pending LLM calls, live JS execution) and restore it on another runtime without waiting for the turn to complete. This enables true live migration under load.

## Ephemeral state inventory

Everything below is **in-memory only** and lost on process death. The provenance graph and task store are durable in shared SurrealDB and do not need checkpointing.

### 1. QuickJS VM heap

**Location:** `QuickJSBridge::runtime` (`Arc<QuickJsRuntimeFacade>`)

The JavaScript VM's closure chains, promise state, `globalThis` bindings, and stack frames live inside the `quickjs_runtime` crate's dedicated OS thread. There is no public API to snapshot or restore this state.

**Checkpoint strategy:**
- **Option A (quickjs-ng):** The quickjs-ng fork exposes `JS_WriteObject` / `JS_ReadObject` for serializing JS values including closures and promise state. This requires either migrating from `quickjs_runtime` to a quickjs-ng binding crate, or adding FFI calls to the snapshot API. **Feasibility: medium.** The `quickjs_runtime` crate wraps the bellard QuickJS engine; quickjs-ng is a maintained fork with snapshot support but a different C API surface.
- **Option B (CRIU):** Process-level checkpoint via CRIU (Checkpoint/Restore In Userspace). Captures the entire process memory including the QuickJS thread. **Feasibility: low for per-agent granularity.** CRIU checkpoints the whole process, not individual threads. In a multi-agent runner, this would checkpoint all agents together.
- **Option C (replay from provenance):** Don't serialize the JS heap. Instead, replay the conversation from the provenance graph on the new runtime until the agent reaches the same execution point. **Feasibility: high for deterministic agents, low for non-deterministic ones.** Requires deterministic LLM responses (cached) and deterministic tool results.

**Recommendation:** Option A for agents that need true mid-turn migration. Option C as a fallback for the common case where replay is acceptable.

### 2. Tool session FSM state

**Location:** `BamlRuntimeState` in `baml-rt-quickjs`

Three `DashMap`s track open tool sessions:

| Map | Key | Value | Serializable? |
|-----|-----|-------|---------------|
| `tool_session_scopes` | `ToolSessionId` (Uuid, serializable) | `ToolSessionScope` { tool_name, scope: RuntimeScope, open_input } | **All fields serializable**, just missing derive |
| `tool_session_states` | `ToolSessionId` | `ToolCallSessionState` { context: ToolCallContext, start: Instant } | **Partially.** `ToolCallContext` fields (tool_name, args, metadata, etc.) are all serializable. `Instant` cannot be serialized — convert to elapsed duration or drop. |
| `tool_session_effect_tokens` | `ToolSessionId` | `EffectStartToken<ToolKind>` | **Not serializable.** Contains live effect channel state. Must be re-created on the target runtime. |
| `read_completion_tool_anchors` | `ToolSessionId` | `ActivityAnchorId` | **Fully serializable.** Both types derive Serialize. |

**Checkpoint strategy:** Serialize `tool_session_scopes`, the serializable fields of `tool_session_states` (excluding `Instant`), and `read_completion_tool_anchors`. On restore, re-create `EffectStartToken`s from the target runtime's effect bus. The tool session FSM can resume from its last acknowledged op.

**Key insight:** The FSM state is explicit (Open, Send, Read, Finish, Abort). The position in the FSM is recoverable from the `ToolCallSessionState`. The underlying I/O connections (HTTP to external services, Claude subprocess) cannot be serialized but can be re-established — tool session protocol supports reconnection by design (each Send/Read is a discrete operation).

### 3. Execution sessions (planning FSM)

**Location:** `execution_sessions: Arc<DashMap<String, ExecutionSession>>` shared across `BamlRuntimeState`, `QuickJSBridge`, and `ToolExecutionContext`.

`ExecutionSession` is an enum with four variants:

```
AwaitIntent   { owner_scope, owner_task_id, owner_context_id }
AwaitPlan     { base, intent_id, epoch }
Executable    { base, intent_id, plan_id, plan_steps, step_status, step_deps,
                completed, epoch, current_step_id }
Closed        { owner_scope, owner_task_id, owner_context_id }
```

**All fields are plain data** (`String`, `Vec<String>`, `HashMap<String, String>`, `HashSet<String>`, `u64`, `Option<String>`, `RuntimeScope`). The type just lacks `#[derive(Serialize, Deserialize)]`.

**Checkpoint strategy:** Add `Serialize`/`Deserialize` derives. This is the most checkpoint-friendly type in the stack — serialization is trivial.

### 4. In-flight LLM calls

**Location:** `QuickJSBridge::eval_results_by_token` (pending BAML/JS call results) and `stream_sessions` (active LLM streams).

When a `__baml_invoke` or `__baml_stream` is in-flight:
- A `tokio::spawn`ed task drives the HTTP request to the LLM provider inside the BAML runtime
- The JS side holds a pending Promise waiting for the result
- Partial stream chunks are buffered in an `mpsc::channel(64)`

**Checkpoint strategy:**
- **Cache-on-arrival:** When an LLM response arrives, write it to a checkpoint cache (keyed by invocation token) before delivering to JS. On restore, if the cached response exists, skip the LLM call and inject the result directly.
- **Truly in-flight (no response yet):** The LLM call must be replayed on the target. The prompt and parameters are in the BAML function call metadata (which is in the provenance graph as `LlmCallStarted`). Replay the call from the provenance record.
- **Streaming in progress:** Partial chunks already delivered to JS are in the provenance graph. On restore, either replay from the beginning (wasting tokens but correct) or resume from the last chunk if the LLM provider supports cursor-based resumption (most don't).

**Recommendation:** For the prototype, only checkpoint at points where no LLM call is actively streaming. This is a natural quiescent sub-point within a turn (between sequential LLM calls, or between an LLM call completing and the next tool session opening).

### 5. Live stream sessions (A2A HTTP/SSE)

**Location:** `A2aAgent::stream_sessions: Arc<Mutex<HashMap<LiveStreamSessionKey, LiveStreamSession>>>`

`LiveStreamSession` holds `turn_tx` / `relay_tx` (channel handles) and `in_flight: bool`. The channels are TCP connection-bound and cannot be serialized.

**Checkpoint strategy:** The SSE connection will drop on migration. The client reconnects (SSE `EventSource` reconnect is native). The new runtime replays missed events from the shared task store. The `LiveStreamSessionKey` (context_id + optional task_id) is the session identity — it must be preserved so the client reconnects to the same logical session.

What to serialize: `LiveStreamSessionKey` → `in_flight` flag. What to abandon: channel handles. The target runtime creates new channels on reconnect.

### 6. Archive reference tables

**Location:** `BamlRuntimeState::archive_ref_tables: Arc<ContextRefTables>`

Ephemeral per-conversation indices (`@N`, `#N`) mapping shorthand references to provenance node IDs. These are built up during a conversation turn.

**Checkpoint strategy:** These are derived from provenance data and can be reconstructed by re-scanning the context's provenance graph. No need to serialize — rebuild on restore.

## Checkpoint struct

```rust
/// Serializable snapshot of an agent's mid-turn execution state.
#[derive(Serialize, Deserialize)]
struct AgentCheckpoint {
    /// Content hash identifying the agent's code version.
    content_hash: DeploymentContentHash,
    /// Agent identity.
    agent_id: AgentId,

    // -- Tool sessions --
    /// Open tool session scopes (tool_name, runtime scope, open args).
    tool_session_scopes: Vec<(ToolSessionId, SerializedToolSessionScope)>,
    /// Tool session FSM states (tool context minus Instant).
    tool_session_states: Vec<(ToolSessionId, SerializedToolCallState)>,
    /// Read-completion anchors.
    read_completion_anchors: Vec<(ToolSessionId, ActivityAnchorId)>,

    // -- Planning --
    /// Execution session FSM states (pure data, fully serializable).
    execution_sessions: Vec<(String, ExecutionSession)>,

    // -- LLM --
    /// Cached LLM responses for in-flight calls (invocation token → response).
    /// On restore, these are injected instead of re-calling the LLM.
    cached_llm_responses: Vec<(String, serde_json::Value)>,

    // -- JS heap (optional, requires quickjs-ng) --
    /// Serialized QuickJS heap snapshot. None if snapshot not available.
    js_heap_snapshot: Option<Vec<u8>>,

    // -- Provenance --
    /// Any provenance events not yet flushed to SurrealDB.
    pending_provenance_events: Vec<ProvEvent>,

    /// Checkpoint timestamp.
    checkpoint_timestamp_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct SerializedToolSessionScope {
    tool_name: String,
    scope: RuntimeScope,
    open_input: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct SerializedToolCallState {
    tool_name: String,
    function_name: Option<String>,
    args: serde_json::Value,
    metadata: serde_json::Value,
    delegation_target: Option<String>,
    elapsed_ms: u64,  // converted from Instant
}
```

## Migration protocol state machine

```
                    ┌─────────────────────┐
                    │       Active        │
                    └──────────┬──────────┘
                               │ CheckpointRequested
                               ▼
                    ┌─────────────────────┐
                    │     Suspending      │ Reject new A2A requests.
                    │                     │ Wait for quiescent sub-point
                    │                     │ (between LLM calls, or at
                    └──────────┬──────────┘  turn boundary if no sub-point).
                               │
                               ▼
                    ┌─────────────────────┐
                    │    Serializing      │ Capture JS heap (if available),
                    │                     │ tool sessions, execution sessions,
                    │                     │ cached LLM responses.
                    └──────────┬──────────┘
                               │
                               ▼
                    ┌─────────────────────┐
                    │   Transferring      │ Write AgentCheckpoint to shared
                    │                     │ SurrealDB (or direct transfer).
                    │                     │ Signal target runner.
                    └──────────┬──────────┘
                               │
                    ┌──────────┴──────────┐
                    │                     │
                    ▼                     ▼
          ┌──────────────┐     ┌───────────────────┐
          │   [target]   │     │   Rollback        │ On any failure:
          │  Restoring   │     │                   │ source resumes
          │              │     │ Source → Active    │ from Active.
          └──────┬───────┘     └───────────────────┘
                 │
                 ▼
          ┌──────────────┐
          │    Active    │ Boot agent from content hash,
          │   (target)   │ inject checkpoint state,
          └──────────────┘ resume JS execution.
```

### State transitions

| From | To | Trigger | Guard |
|------|----|---------|-------|
| Active | Suspending | `CheckpointRequested` via control API | Agent exists and is Active |
| Suspending | Serializing | Quiescent sub-point reached | No active LLM stream; tool sessions at FSM boundary |
| Suspending | Active (rollback) | Timeout (configurable, default 30s) | |
| Serializing | Transferring | Checkpoint written successfully | |
| Serializing | Active (rollback) | Serialization failure (e.g., JS snapshot not supported) | |
| Transferring | Restoring (target) | Target runner acknowledges checkpoint receipt | |
| Transferring | Active (rollback) | Target unreachable or rejects | |
| Restoring | Active (target) | Agent booted + checkpoint injected + JS resumed | |
| Restoring | Active (source, rollback) | Restore failure on target | |

### Rollback guarantee

At every stage, the source runner retains the agent in a suspended-but-recoverable state until the target confirms successful restoration. The source only removes the agent after receiving the target's `RestoreComplete` acknowledgment. This ensures no data loss even on network partition — the agent resumes on the source.

## Quiescent sub-points

A full turn (user message → LLM call → tool execution → response) contains natural quiescent sub-points where checkpointing is cleanest:

1. **Between sequential LLM calls:** JS has processed the previous LLM result but hasn't started the next call yet. Tool sessions from the previous call are Finished. The JS heap is quiescent (no pending Promises from BAML).

2. **After tool session Finish, before next BAML call:** The tool session FSM is at a terminal state. The JS code is about to call the next BAML function.

3. **During `awaitInput` suspension:** The agent has emitted `INPUT_REQUIRED` and is suspended waiting for the next user message. This is the cleanest sub-point — no active execution at all, just a suspended JS Promise.

Sub-point 3 (`awaitInput`) is essentially a turn boundary and is already handled by the existing drain mechanism. Sub-points 1 and 2 are the targets for true mid-turn checkpoint.

**Detection:** The runtime can detect quiescent sub-points by monitoring:
- `in_flight_invoke_count == 0` (no active BAML calls)
- `stream_sessions` empty (no active LLM streams)
- All `ToolCallSessionState` entries at `Finished` or `Aborted`

## Hard blockers and mitigations

### 1. QuickJS heap snapshot

**Blocker:** The `quickjs_runtime` crate (bellard engine) does not expose the C-level `JS_WriteObject`/`JS_ReadObject` API. The Rust wrapper provides no snapshot mechanism.

**Mitigation options:**
- Fork `quickjs_runtime` and add snapshot FFI bindings
- Switch to a quickjs-ng binding crate that exposes snapshot
- Use CRIU for whole-process checkpoint (coarse but works)
- Accept replay-based restoration (no heap snapshot needed, but requires deterministic execution or cached LLM responses)

**Assessment:** For the prototype, replay-based restoration is sufficient. The JS heap at quiescent sub-points contains mostly the agent's top-level state (which is reconstructable from provenance) plus the pending `awaitInput` Promise chain (which is trivially recreated by re-sending the conversation history).

### 2. Non-deterministic JS execution

**Blocker:** JS `Math.random()`, `Date.now()`, timers — if the agent uses these, replay produces different results.

**Mitigation:** Agentium OS agents don't use raw JS timers (QuickJS is configured without timer APIs). `Date.now()` can be intercepted via `globalThis` override. `Math.random()` is rarely used in agent code (LLM results dominate randomness).

### 3. Open HTTP connections in tool sessions

**Blocker:** Tools like Claude session (`ClaudeSdkSource`) maintain a subprocess connection. This cannot be serialized.

**Mitigation:** Tool sessions are FSM-based. On restore, re-open the tool session from its last acknowledged FSM state. The Claude session SDK supports session resumption via `sdk_session_id` stored in `AgentWorkspaceRegistry`. Other HTTP tools (ClickUp, Notion, Slack) are stateless per-call — no connection to preserve.

### 4. BAML runtime internals

**Blocker:** `BamlExecutor`, `BamlRuntime`, `ClientRegistry` — these are opaque types from the BAML git dependency. They cannot be serialized.

**Mitigation:** These are configuration-derived, not execution-derived. They are rebuilt from the agent's BAML schema + LLM config at boot time. On restore, boot the agent normally (same as deploy), then inject the checkpoint state into the freshly-built runtime.

## Why turn-boundary is the right first step

At turn boundary:
- JS is idle (no pending Promises)
- No active LLM calls
- All tool sessions complete
- All provenance flushed to shared SurrealDB
- The only "state" is the conversation history, which IS the provenance graph

Mid-turn checkpoint adds complexity for marginal operational value in most scenarios. The turn boundary is a natural quiescent point that the existing drain mechanism already exploits. Mid-turn is worth pursuing for:
- Long-running agent turns (multi-minute tool orchestrations)
- Urgent node evacuation (hardware failure imminent)
- Demonstrating architectural superiority over process-pinned platforms

The code changes in this sprint (shared SurrealDB, drain mechanism, cluster routing, migration API) are prerequisites for both turn-boundary and mid-turn migration. The mid-turn extension layer (checkpoint struct, quiescent detection, JS snapshot) builds on top without changing the foundation.
