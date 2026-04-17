# Tool Sandbox Execution Plan (Process + Sandbox + Wasm)

**Status:** proposal (discussion only, no code changes yet)  
**Date:** 2026-04-17  
**Scope:** tool execution architecture in `baml-rt-tools` and runner integration  
**Related:** `external-tools-protocol-first.md` (this document extends that plan)

---

## RFC summary (quick review)

### Decisions (proposed)

1. Backend model: `ToolBackend::{Static, External, Sandbox, Wasm}`. (MCP reserved in design only — not added to the enum until Phase F; see decision #10.)
2. `External` keeps current meaning (child process + stdio JSON-RPC) for compatibility.
3. External metadata gains optional `runtime`; missing `runtime` defaults to process (`#[serde(default)]`).
4. Sandbox workload transport uses minimal TSRPC (`Describe`/`Invoke`), not MCP.
5. Sandbox infra is shared; tool protocol is separate from future full-agent ACP-like protocol.
6. Sandbox runtime identity digest is bound in lockfile/cache/provenance (`ExternalToolLockEntry.runtime_digest: Option<String>`).
7. Sandbox lifetime is keyed by `(agent_instance_id, context_id, tool_id)`, created lazily on first invoke; one sandbox per tool per context (no cross-tool sharing, no cross-instance sharing).
8. Sandbox infra is accessed through a `SandboxProvider` trait. First implementation: `MicrosandboxProvider`, a thin wrapper over the embedded `microsandbox` crate (no external daemon).
9. Sandbox internal state is a black box to the runtime — tool-adapter owns any reset/cleanup between invokes.
10. `ToolBackend::Mcp` is **reserved in design only** as a placeholder for future MCP-server integration. Not added to the enum until Phase F begins, to avoid exhaustive-match churn. Default role when implemented will be client-only (runtime connects to externally-managed MCP servers; server lifecycle out of scope).
11. Secrets: runner resolves plaintext (existing Fnox path). A given secret binding is **either** create-time egress-bound (default for HTTP tools) **or** per-invoke TSRPC-carried (default for non-HTTP auth) — never both for the same binding. Canonical rules in §10.1.
12. Sandbox capacity caps are native via microsandbox `idle_timeout`, `max_duration`. Reattach via `Sandbox::list()` / `Sandbox::get(name)` is scoped to **in-process lifetime only** in v1 (hot reload, same-process cache rebuild). Cross-process-restart reattach is out of scope: runner_id is a fresh UUID per boot (no persistence), so a restarted runner starts with a cold cache; microsandbox's `idle_timeout` reaps the previous runner's orphans in the background.

### Why now

- Current process path is strong for trusted/local DX, but weak for isolation goals.
- We need additive architecture (no regressions, no forced migration).
- Designing for dev-agent-hardest-case avoids dead-end “simple-only” designs.

### Rollout gates

- **Gate A:** compatibility (all existing fixtures green, no metadata edits required).
- **Gate B:** abstraction (process path unchanged on shared invoker trait).
- **Gate C:** sandbox MVP (`SandboxInvoker` + microsandbox adapter parity on `Describe/Invoke`).
- **Gate D:** hardening (lifecycle events, quarantine/backoff parity, cleanup guarantees).
- **Gate E:** DX (scaffold/runtime validator/docs finalized).

---

## 1) Why this document

We already have a working external tool path:
- `ToolBackend::External`
- child process spawn (`tool-server`)
- JSON-RPC over stdio (`tool/describe`, `tool/invoke`)
- metadata from `tool-metadata.json`
- dynamic registration via resolver/registry

This is good for trusted/local development. It is not sufficient for stronger isolation (microVM/container per invoke/session).

This plan adds sandboxed execution as a new execution layer without breaking the current flow.

---

## 2) Design principle + scope

### Design principle (hardest-case first)

Design for **dev-agent-like workloads in a sandbox** (Claude/Aider class):
- long-running behavior
- constrained credentials/network
- high auditability
- strict isolation

If this works, simple “run once and return report” tools are a subset.

### Goals

1. Keep process external tools working unchanged.
2. Add sandboxed execution as first-class backend.
3. Preserve existing tool FSM semantics (`Open/Send/Read/Finish/Abort`).
4. Keep contract minimal (`Describe`/`Invoke`) and transport-agnostic.
5. Leave room for Wasm backend.
6. Avoid tool/agent sandbox protocol overlap.

### Non-goals (this phase)

1. No removal of current JSON-RPC process path.
2. No forced migration of existing tools.
3. No single protocol for both tools and full agents.
4. No implementation in this document.

---

## 3) Current anchor

- Current enum in `crates/baml-rt-tools/src/tools.rs`: `ToolBackend::{Static, External, Wasm}`
- Existing external stack:
  - `external_tools::ExternalInvoker`
  - `StdioSubprocessInvoker`
  - `ProcessToolHandler`
  - `DevModeResolver`
  - `schemas/external_tool_metadata.schema.json`

Practical meaning today: `External` == process + stdio.

---

## 4) Architecture decisions

### 4.1 Backend enum

```rust
pub enum ToolBackend {
    Static,
    External, // current process backend
    Sandbox,  // microsandbox/container-backed backend
    Wasm,
    // Mcp — reserved in design only (see §11 Phase F). NOT added to the enum yet,
    // to avoid exhaustive-match churn in downstream code until the backend is actually
    // implemented. When Phase F starts, add the variant (possibly gated behind
    // `#[cfg(feature = "mcp-backend")]` or `#[non_exhaustive]` at that point).
}
```

Rationale: preserve compatibility while making sandbox explicit. MCP backend stays at the *design* level for now — no enum churn without functionality.

### 4.2 Runtime block in metadata

Move from implicit process launch to explicit runtime declaration.

```json
{
  "runtime": {
    "kind": "process",
    "command": ["./tool-server"]
  }
}
```

Sandbox example (digest-pinned image):

```json
{
  "runtime": {
    "kind": "sandbox",
    "image": "ghcr.io/org/dev-echo@sha256:...",
    "entrypoint": ["/app/tool-adapter"]
  }
}
```

#### Compatibility rules

1. `runtime` is optional with `#[serde(default)]`.
2. Missing `runtime` => process mode.
3. Relative paths are resolved relative to directory containing `tool-metadata.json`.
4. Transport is provider-owned for the `sandbox` kind (v1: TSRPC over microsandbox `exec_stream` stdio; future alternatives possible behind the `SandboxProvider` trait). The `process` kind implicitly uses stdio.

#### Wrapper vs command decision (resolved)

**Chosen path: (b) wrapper kept as default (`command: ["./tool-server"]`)**.

- Existing scaffolds and tools continue to work unchanged.
- Process runtime still allows direct commands (`["python", "main.py"]`, `["node", "dist/server.js"]`) for advanced users.
- Wrapper deprecation (if any) is a later policy decision, not part of this migration.

#### Command execution semantics

- `command` is argv (direct exec); no shell expansion. For shell features, use `["bash", "-c", "..."]`.
- CWD is the tool package directory (same as metadata path resolution, rule 3).
- Runner injects `BAML_*`-prefixed environment variables; spawned process inherits them.
- Metadata MAY declare a required env-var allowlist for documentation/validation; Phase A treats this as advisory, later phases may enforce.

#### Setup hooks (optional)

```json
{
  "runtime": {
    "kind": "process",
    "setup": ["python -m venv .venv", ".venv/bin/pip install -r requirements.txt"],
    "command": ["./tool-server"]
  }
}
```

Setup execution semantics:

1. **Applies to `process` kind only.** Sandbox images are pre-built artifacts — setup has no role there; the image IS the immutable artifact (see §8.4).
2. Runs at **deploy time** (tool registration), not per invoke. Results cached across invokes within a deploy.
3. Commands MUST be idempotent (rerunnable without destructive side effects).
4. **Artifact/scratch split (digest integrity):** the tool package dir is digest-verified and treated as read-only at runtime. Setup writes *only* into a runner-provided scratch dir (exposed via `$BAML_TOOL_SCRATCH_DIR`); the artifact dir is exposed read-only via `$BAML_TOOL_ARTIFACT_DIR`. Digest scope covers the artifact dir only; scratch contents are never digested, never shipped.
5. CWD during setup is the scratch dir.
6. Policy-controlled: dev-friendly by default; production deployments typically restrict or disable `setup`.
7. **Timeouts:**
   - per-command timeout: runner-configurable, default 120 s
   - total setup timeout: runner-configurable, default 600 s
   - on timeout, command is SIGKILL'd; setup fails with `SetupFailed` (see §13)
8. **Output capture limits:** stdout + stderr captured up to 1 MB per command; overflow truncated with a `[truncated]` marker and logged. Full output available in debug logs at deploy time only.
9. **Classification:** setup failures surface as `SetupFailed` (configuration error class); non-transient, no auto-retry.

---

## 5) Sandbox protocol (TSRPC)

### 5.1 Why not MCP here

Sandbox **workload transport** does not use MCP because:
1. Existing tool lifecycle is already modeled (`Describe`/`Invoke`).
2. MCP would add extra scope and protocol coupling not required for tool parity.
3. We need minimal contract to preserve existing metadata/policy/provenance model.

> **Scope note:** this decision is about the sandbox workload protocol (runtime ↔ tool-adapter). It does **not** reject MCP as a tool source. An `Mcp` backend is **reserved in design only** (§11 Phase F) — the enum variant will be added when implementation starts, not before, to avoid exhaustive-match churn. Default role when implemented will be client-only (connecting to externally-managed MCP servers).

### 5.2 Wire contract

TSRPC mirrors current JSON-RPC message shapes for:
- `tool/describe`
- `tool/invoke`

Transport framing (sandbox path, v1):
- **Length-prefixed JSON frames** carried over `microsandbox::Sandbox::exec_stream` stdio (guest `tool-adapter` reads stdin, writes stdout; `ExecSink` + `ExecHandle::recv` on the host side).
- Frame header: 4-byte big-endian unsigned length, followed by the JSON payload of exactly that many bytes. No trailing newline or delimiter.
- Chosen over newline-delimited JSON for defense: `exec_stream` chunks may arrive in arbitrary sizes, and a buggy adapter could pollute stdout; length-prefixed framing is binary-safe and unambiguous in both failure modes.
- Payload structure is transport-agnostic; future migration to microsandbox Events API (`/run/agent.sock`) or another channel is possible without schema changes.

This prevents re-arguing message schema while changing transport substrate.

### 5.3 Channel separation (v1 mapping)

| Channel | v1 implementation |
|---|---|
| **Control** | Framed TSRPC (length-prefixed JSON, §5.2) on the tool-adapter's stdin/stdout, carried via `exec_stream`. |
| **Logs** | Tool-adapter writes to stderr; runner collects via `ExecEvent::Stderr` stream on the same `ExecHandle`. Never multiplexed with control. |
| **Metrics** | Host-side OpenTelemetry only in v1. The sandbox does not emit metrics to the runner; the runner observes externally (latency, failure counts, sandbox state). Guest-side metrics are the tool-adapter author's concern and out of scope. |

Future options: microsandbox Events API (`/run/agent.sock`) would give a fourth channel for structured bidirectional events; adopt when it GAs.

This separation is what prevents the stdio/log multiplexing problems common in container execution — control has exclusive ownership of stdout.

---

## 6) Avoiding protocol overlap (tools vs full agents)

Two-layer split:

1. **Sandbox orchestrator layer** (shared infra)
   - create/teardown sandbox
   - credential and policy injection
   - resource/egress enforcement

2. **Workload protocol layer** (separate)
   - Tool: TSRPC `Describe/Invoke`
   - Agent: ACP-like protocol (future, separate design)

Microsandbox is an **embedded Rust crate**, not a daemon. The runner links it directly; there is no separate service. The crate, when it creates a sandbox, spawns a *sandbox child process* on the host that runs the VM, networking, and relay; inside the VM, `agentd` exposes `exec` / `fs` / events. Three layers:

```
 ┌─────────────────────────────────────────────────────────────────┐
 │                   Runtime host (runner process)                 │
 │                                                                 │
 │   SandboxInvoker ──── microsandbox::Sandbox API ────► (create,  │
 │         │                    [embedded crate]         exec,     │
 │         │                                             secret,   │
 │         │                                             network)  │
 │         │                                                       │
 │         │            TSRPC over exec_stream stdio               │
 │         └─────────────────────────────────────────────► ...     │
 └─────────┬───────────────────────────────────────────────────────┘
           │  (child process spawn)
           ▼
 ┌─────────────────────────────────────────────────────────────────┐
 │   microsandbox sandbox process  (one per sandbox, on host)      │
 │   • runs the microVM + host networking stack                    │
 │   • relays exec / fs / events between app and guest agent       │
 │   • enforces idle_timeout, max_duration                         │
 │   • up to 16 concurrent clients can attach                      │
 └─────────┬───────────────────────────────────────────────────────┘
           │  (microVM boundary)
           ▼
 ┌─────────────────────────────────────────────────────────────────┐
 │   Guest microVM                                                 │
 │   ┌──────────────────┐   ┌──────────────────────────────────┐   │
 │   │   agentd (PID 1) │   │   tool-adapter                   │   │
 │   │ exec / fs / sock │◄─►│   (reads JSON-RPC on stdin,      │   │
 │   │  /run/agent.sock │   │    writes on stdout; runs        │   │
 │   │                  │   │    author tool logic)            │   │
 │   └──────────────────┘   └──────────────────────────────────┘   │
 └─────────────────────────────────────────────────────────────────┘
```

- **Provider API** (runtime ↔ microsandbox crate): *lifecycle* — `create`, `exec_stream`, `secret`, `network`, `stop`, `list`, `get`.
- **TSRPC** (runtime ↔ tool-adapter inside VM): *workload* — `Describe`, `Invoke`, carried as length-prefixed JSON over `exec_stream` stdio in v1.
- Runtime communicates workload over the provider-exposed channel; the microsandbox sandbox process may be in the physical path as a relay, but is not the logical workload protocol authority.

---

## 7) Rust interface proposal

### 7.1 Runtime tagged enum (no Option bag)

```rust
pub enum ToolRuntime {
    Process(ProcessRuntimeSpec),
    Sandbox(SandboxRuntimeSpec),
    Wasm(WasmRuntimeSpec),
}
```

### 7.2 Invoker abstraction

```rust
#[async_trait]
pub trait ToolInvoker: Send + Sync {
    async fn describe(&self, tool: &ToolName, timeout: Duration) -> Result<ToolDescribe>;
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeResponse>;
}
```

Implementations:
- `ProcessInvoker`
- `SandboxInvoker`
- `WasmInvoker` (future)

Note: reuse existing `external_tools::*` types (`ToolDescribe`, `InvokeRequest`, `InvokeResponse`) where possible; rename only at abstraction boundary if needed.

### 7.3 SandboxProvider abstraction

Sandbox lifecycle lives behind a trait; runtime never calls microsandbox APIs directly. Swappable for tests and future alternatives. `SandboxSpec` fields align 1:1 with the actual microsandbox `SandboxBuilder` (see the microsandbox crate docs), so the first impl is a thin passthrough.

```rust
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle>;
    async fn rpc_channel(&self, handle: &SandboxHandle) -> Result<TsrpcChannel>;
    async fn teardown(&self, handle: &SandboxHandle) -> Result<()>;
    fn events(&self, handle: &SandboxHandle) -> BoxStream<'_, SandboxEvent>;

    // In-process reattach / cache rebuild (hot reload, tests, liveness checks).
    // Cross-process-restart reattach is out of scope in v1 (§9.4).
    async fn list_owned(&self, runner_id: &str) -> Result<Vec<SandboxHandle>>;
    async fn reattach(&self, name: &str) -> Result<SandboxHandle>;
}

pub struct SandboxSpec {
    pub name: String,                    // encodes (agent_instance, context, tool); see §9.2
    pub image: ImageDigest,              // digest-pinned OCI ref (see §8.4)
    pub cpus: u32,                       // microsandbox default 1
    pub memory_mib: u32,                 // microsandbox default 512
    pub env: BTreeMap<String, String>,
    pub volumes: Vec<VolumeMount>,       // bind (readonly by default) / named / tmpfs
    pub port_mappings: Vec<PortMapping>, // host↔guest
    pub network_policy: NetworkPolicy,   // maps to microsandbox NetworkPolicy (§10.2)
    pub secrets: Vec<SecretBinding>,     // { env_var, value, allow_hosts } (§10.1)
    pub scripts: BTreeMap<String, String>, // /.msb/scripts/ entries (optional)
    pub idle_timeout: Duration,          // native microsandbox knob
    pub max_duration: Duration,          // native microsandbox knob
    pub detached: bool,                  // true = survive runner exit; v1 default true
    pub pull_policy: PullPolicy,         // Always / IfMissing / Never
}
```

**First implementation: `MicrosandboxProvider`.** Thin wrapper over `microsandbox::Sandbox::builder(...)`. Calls map directly:
- `create` → `Sandbox::builder(spec.name).image(...)...create_detached().await` (or `.create()` when not detached)
- `rpc_channel` → `sandbox.exec_stream(entrypoint, ...)` → returned `ExecHandle` wraps stdin/stdout as a TSRPC `Channel`
- `teardown` → `sandbox.stop().await` with `kill().await` fallback on timeout
- `list_owned` → `Sandbox::list()` filtered by our naming convention (§9.2)
- `reattach` → `Sandbox::get(name)`
- `events` → subscribes to microsandbox lifecycle events for this handle

**Caveat:** microsandbox is beta ("expect breaking changes"). The trait boundary isolates runtime code from that churn. Updates to the crate land in one place: `MicrosandboxProvider`.

**Version pin (v1):** `microsandbox = "=0.3.13"` (exact match). Rationale: beta crate, SDK churn expected; an exact pin prevents silent drift from `cargo update`. Bumps go through explicit review + `MicrosandboxProvider` regression pass (Workstream B in §18).

Mocks / alternates:
- `MockSandboxProvider` for unit tests (no VM, in-memory frames).
- Possible future: `DockerSandboxProvider` for local dev where KVM unavailable.

---

## 8) Metadata + lockfile evolution

### 8.1 Metadata extension

`ExternalToolMetadata` adds `runtime` with `#[serde(default)]`.

### 8.2 Schema extension details

Because schema has `additionalProperties: false`, `runtime` **must** be added to top-level `properties` explicitly (and not required) to preserve compatibility.

**Conditional requirement (sandbox kind):** when `runtime.kind == "sandbox"`, `runtime_digest` is required. Express via JSON Schema `if`/`then`:

```json
{
  "if":   { "properties": { "runtime": { "properties": { "kind": { "const": "sandbox" } } } } },
  "then": { "required": ["runtime_digest"] }
}
```

This is what prevents a sandbox-kind metadata file from passing validation without a digest pin, closing the loophole where §8.4's "MUST be digest-pinned" rule could be violated at the schema level.

### 8.3 Capabilities/egress declaration

Current metadata already has free-form `capabilities`. In this plan:
- Phase A keeps this field and clarifies requested egress/capabilities usage.
- Later phases may tighten shape for sandbox policy enforcement.

### 8.4 Runtime identity pinning

Sandbox runtime image/artifact MUST be digest-pinned.
- Accept: `image@sha256:...`
- Reject: tag-only refs (`:latest`, `:v1` without digest)

Lockfile/cache/provenance must include runtime identity digest. Suggested lock entry extension:
- `runtime_digest: Option<String>` (required for sandbox entries)

Describe cache key:

`(tool_name, tool_digest, runtime_identity_digest)`

---

## 9) Sandbox execution lifecycle

### 9.1 Identity hierarchy (anchor)

The runtime already has a three-level identity hierarchy that sandbox lifetimes must map onto:

```
context_id              ← full conversation; spans all turns/agents/tools
  ├─ task_id            ← one per turn (ephemeral; comes and goes)
  │    └─ tool-session  ← FSM: Open → Send/Read → Finish/Abort
  └─ task_id ...
```

Not every turn calls a sandboxed tool. Sandbox lifetime therefore cannot bind to `task_id` (too short, gaps) nor to `tool-session` (one invoke — no reuse benefit).

### 9.2 Sandbox key + lifetime

**Sandbox key:** `(agent_instance_id, context_id, tool_id)`.

Agent identity is required in the key: two deploys of the same agent package are distinct trust boundaries (potentially different credentials, policies, versions). Context-scope alone is not enough to isolate them.

**Sandbox name convention:** encode the key into the sandbox name so microsandbox itself provides the lookup/reattach primitive:

```
baml:<runner_id>:<agent_instance_id>:<context_id>:<tool_id>
```

**`runner_id`**: UUID generated at runner boot, held in memory only (no cross-restart persistence). Prevents sandbox name collisions when multiple runner processes share a microsandbox home directory (e.g., dev environments, multi-pod deployments sharing a host mount). A fresh restart gets a fresh UUID; the previous runner's sandboxes become unreachable to the new runner and are reaped by microsandbox's `idle_timeout` / `max_duration` in the background.

(Short-hash the components if the composed name is too long for microsandbox's name field — exact encoding is a Phase C detail.)

**Lifetime:** first-use of `tool_id` within `(agent_instance_id, context_id)` → until context close **or** native `idle_timeout` **or** native `max_duration` (see §10.3).

**Isolation:** one sandbox per tool per (agent_instance, context). An agent using Slack + ClickUp within one context runs two sandboxes, never shared.

**Strict precedence rule:** if `ExternalSessionPolicy::Strict` is set on the tool, sandbox-per-invoke applies regardless of cache hits. Every invoke creates + tears down a fresh sandbox. This matches process-backend semantics for paranoid/one-off workloads and overrides the `(agent_instance, context, tool)` caching policy below.

Typical flow (default multi-send policy):

```
Turn 5 ──► tool=slack   ─► lookup (agtA, ctx123, slack)   ─► miss ─► provider.create()   ┐
                                                                                         │ cache
Turn 7 ──► tool=slack   ─► lookup (agtA, ctx123, slack)   ─► hit  ─► reuse handle        │
Turn 8 ──► tool=clickup ─► lookup (agtA, ctx123, clickup) ─► miss ─► provider.create()   │
                                                                                         │
ctx123 ends ──────────────────────────────────────────────────────► provider.teardown(…) ┘
```

Runtime state:

```rust
HashMap<(AgentInstanceId, ContextId, ToolId), SandboxHandle>
```

The FSM `session_id` (Open/Send/Read) is a pointer that resolves to this key; runtime routes TSRPC to the cached handle.

### 9.3 Per-invoke flow (once sandbox exists)

1. resolve metadata/runtime
2. lookup `(agent_instance_id, context_id, tool_id)` in sandbox cache
   - miss → `provider.create(spec)` → store handle → **lazy first-use** (§9.4)
   - hit → reuse handle
3. `Describe` (cached by `(tool_name, tool_digest, runtime_identity_digest)`)
4. `Invoke` over TSRPC on cached handle
5. collect result
6. emit provenance/lifecycle events
7. do **not** teardown — keep handle alive for next invoke

Teardown triggers (any of):
- context close
- idle timeout reached
- max lifetime reached
- `SandboxTerminatedUnexpectedly` (close FSM as error; next call re-creates)

### 9.4 First-use creation (lazy) + in-process reattach

**Lazy create:** sandbox is created on **first invocation** of the tool within the `(agent_instance, context)` scope. No eager pre-warming in v1. Microsandbox cold-start is ~100 ms which makes lazy acceptable for interactive UX.

**Reattach scope (v1): in-process only.**

Cross-process-restart reattach is **out of scope** in v1. `runner_id` is a fresh UUID per runner boot with no persistence (§9.2), so a restarted runner will not find sandboxes it created before the restart. Microsandbox's `idle_timeout` reaps the previous runner's orphans in the background. Every context in a restarted runner pays cold-start on its next tool use.

In-process reattach use cases (still supported via `list_owned` + `reattach`):
1. **Hot reload of sandbox provider config** without a process restart.
2. **Test infrastructure**: deterministic cache rebuild across fixture boundaries within one process.
3. **Handle liveness checks**: confirm a cached handle still points at a running sandbox mid-operation.

Detached mode (`create_detached()`) is still used by default so a graceful runner shutdown doesn't cascade-kill sandboxes; microsandbox reaps them via `idle_timeout`/`max_duration`.

If in-flight UX pain from cold-starts after restart becomes a measured problem later, persistent `runner_id` + cross-restart reattach is a localized follow-up change.

**Reattach validation checklist** — every reattached handle must pass all checks or be torn down:
1. **Status check**: sandbox is in `Running` state (not `Draining`, `Paused`, `Stopped`, `Crashed`).
2. **Context liveness**: the encoded `context_id` is still in the runner's active context set.
3. **Runtime digest match**: the sandbox's creation-time `runtime_digest` (stashed at create via a well-known env var / metadata field) matches the current tool metadata's digest. Catches the "tool image rev'd without new agent_instance_id" case.
4. **Policy hash match**: a stable hash over the effective `NetworkPolicy` + secret allowlist + resource limits, stashed at create time, matches the current effective policy. Catches "config changed but agent_instance didn't" case.
5. **Age check**: `now - created_at < max_duration` (microsandbox will kill soon anyway; don't reuse a nearly-dead sandbox).

Any failed check → `provider.teardown(handle)` + evict from cache. Next invoke cold-creates.

On `SandboxTerminatedUnexpectedly` during normal operation, the cache entry is evicted; next invoke pays cold-start.

Not trying to persist the handle map runner-side — microsandbox's own registry is the source of truth for in-process reattach in v1. (Cross-process-restart reattach is out of scope; a restarted runner does not consult the registry.)

Future (post-v1): opt-in pre-warming, predictive warm pools.

### 9.5 Session policy interaction

- `ExternalSessionPolicy::Strict` → sandbox-per-invoke. **Overrides the `(agent_instance, context, tool)` cache** (see §9.2 precedence rule). For paranoid/one-off workloads or when tool metadata marks the session strict.
- `ExternalSessionPolicy::MultiSend` → sandbox-per-`(agent_instance, context, tool)`. **Default for sandbox kind.**

Why warm by default, given ~100 ms microVM boot: the reuse win is not primarily VM boot amortization. It's **tool-adapter state**: cached HTTP clients, warmed TLS sessions, connection pools, auth token caches, and any per-tool initialization the author does on startup. Chatty tools (`memory`, agent-style tools) pay that state cost many times under Strict and once under MultiSend. Strict forfeits that reuse for fresh-state guarantees — the right trade for security-sensitive or adversarial-code tools, but wrong as the blanket default.

---

## 10) Security model (decisions)

### 10.1 Secrets

Microsandbox provides native **placeholder-substitution egress injection**: the VM never sees the real credential. A random placeholder (`msb_ph_...`) is injected as an env var; when the VM makes an outbound request to a host declared in `allow_host(...)`, microsandbox swaps the placeholder for the real value in-flight. Everywhere else the placeholder is a meaningless string — even full code execution inside the sandbox has nothing to steal.

**Primary path (HTTP tools):** use microsandbox secret bindings.
1. **Runner resolves plaintext once** via the existing Fnox path (keeps the current trust boundary — this is Option A from design review: runner resolves, provider injects).
2. Runner calls `.secret(|s| s.env("TOKEN").value(plaintext).allow_host("api.example.com"))` at `create()` time.
3. Microsandbox caches the placeholder mapping for the sandbox lifetime; no runner involvement per invoke.
4. Plaintext stays on the runner only during the `create()` call, never enters the VM, never persists in microsandbox state exposed to the guest.

**Fallback (non-HTTP auth):** if a tool needs credentials the egress proxy can't carry (DB connection string, SDK stdin auth, gRPC metadata not plumbed by microsandbox), fall back to **TSRPC-carried credentials per invoke**:
1. Runner resolves plaintext per invoke.
2. Credentials are attached to the `Invoke` TSRPC payload.
3. Tool-adapter uses them for the call, never persists them across invokes.
4. Use only when option (c) doesn't apply.

**Metadata:** declares required credentials and their binding style (host-bound for egress injection, vs invoke-carried). Phase A keeps the schema advisory; Phase C enforces.

**Not used in v1:** `refresh_credentials(handle, creds)` provider-side live rotation — too much provider surface for too little gain.

### 10.1a Canonical binding rule

A given secret binding declared in tool metadata is **either**:
- **create-time egress-bound** — injected via microsandbox `.secret(...)` at sandbox creation; domain-scoped via `allow_host(...)`; never seen by the VM as plaintext; valid for the sandbox's lifetime. **Default for HTTP tools.**

— or —

- **per-invoke TSRPC-carried** — runner resolves plaintext per invoke; credential attached to the `Invoke` payload; tool-adapter uses it transiently; never persisted. **Default for non-HTTP auth.**

**Never both for the same binding** in v1 — mixing is reserved for a later phase via explicit metadata opt-in. This keeps the secret lifetime model unambiguous per binding and auditable in provenance.

### 10.1b Credential rotation (operational)

Create-time egress-bound secrets (default HTTP path) are injected **once**, at sandbox creation, and persist for the sandbox's lifetime. Consequences:

- **Credential rotation requires sandbox recycle.** If Fnox rotates the upstream token, existing warm sandboxes continue to carry the old value until teardown.
- **Natural rotation boundaries:**
  - context close (teardown + recreate on next context)
  - `idle_timeout` expiry (microsandbox drains, next invoke cold-creates with fresh secret)
  - `max_duration` expiry (hard lifetime cap, same outcome)
- **Forced rotation**: operator calls `provider.teardown(handle)` on the affected sandboxes; next invoke re-creates with the current secret value. Not automated in v1.
- **Per-invoke TSRPC-carried secrets** (non-HTTP fallback) rotate implicitly per call with no sandbox churn.

`refresh_credentials(handle, creds)` live rotation remains out of v1 scope.

### 10.2 Egress/capabilities authority

Maps directly onto microsandbox `NetworkPolicy`.

- Metadata declares **requested** capabilities (destinations, protocols, ports).
- Runner/orchestrator policy computes **effective authorized** `NetworkPolicy`.
- Default action: `Deny`. Explicit rules enumerate allowed outbound destinations (`Destination::Any` / domain / IP range) with protocol + port constraints.
- Built-in blocks: `DestinationGroup::Metadata` and `DestinationGroup::Private` are denied by default on top of whatever the tool declares.
- DNS interception is on: domain-level block rules, DNS-rebinding protection, and secret↔domain IP binding (prevents TOCTOU between policy check and connection).
- Effective policy recorded in provenance alongside the sandbox handle.

### 10.3 Additional controls

- resource limits (cpu/mem via microsandbox `cpus`/`memory`, POSIX rlimits per-exec)
- timeout + hard kill (`timeout()` on per-exec, `kill()` on sandbox)
- filesystem constraints (bind mounts default to readonly; volumes explicit)
- backend/runtime identity capture in provenance

**Sandbox lifetime knobs** (native microsandbox primitives — runner wires them through):

| Knob | Microsandbox mapping | Purpose | Suggested default |
|---|---|---|---|
| `idle_timeout` | `SandboxBuilder::idle_timeout(sec)` (native auto-drain) | Drain after N min with no ops. Frees resources for idle conversations. | 300 s (5 min) |
| `max_duration` | `SandboxBuilder::max_duration(sec)` (native lifetime cap) | Hard upper bound regardless of activity. Prevents leaks, credential staleness. | 3600 s (1 h) |

Both passed via `SandboxSpec` (§7.3) to the provider. Metadata MAY override on a per-tool basis in later phases; v1 keeps them runner-global.

**Capacity safety** (runner-enforced, above microsandbox layer):

| Cap | Purpose | v1 behavior |
|---|---|---|
| `max_sandboxes_per_agent_instance` | Bound resource use per deploy | fail-fast on admission |
| `max_sandboxes_per_context` | Fairness across contexts on a shared runner | fail-fast on admission |

Admission policy is a named mode (runner-configurable):

| Mode | Behavior | Status |
|---|---|---|
| `FailFast` | Reject new sandbox creations when a cap is hit; surface `CapacityExceeded`. | **v1 only** |
| `EvictIdle` | Evict the least-recently-used sandbox with no active invoke in flight to make room. | Post-v1 |
| `Queue` | Hold the create request until a slot frees. | Post-v1 (batch workloads) |

v1 ships `FailFast` only. Evicting an in-use warm sandbox is strictly worse UX than refusing the new one, and distinguishing "active" vs "truly idle" reliably requires signals (active invoke count, last-use timestamp) that should ride on the cache in a later phase before any eviction is attempted.

---

## 11) Implementation phases

> This plan layers after current external-process maturity from `external-tools-protocol-first.md`, and before/alongside that document’s Phase 4 direction.

### Phase A — Model + compatibility

1. Add `ToolBackend::Sandbox`.
2. Add metadata `runtime` with `#[serde(default)]`.
3. Extend schema (`runtime` in `properties`).
4. Keep default behavior when `runtime` absent.

Acceptance:
- all existing fixtures green with unchanged metadata
- no behavior change for process tools

### Phase B — Invoker abstraction

1. Introduce `ToolInvoker` abstraction.
2. Adapt existing stdio invoker as `ProcessInvoker`.
3. Preserve planner/runtime behavior.

### Phase C — Sandbox invoker + microsandbox integration

1. Add `microsandbox = "=0.3.13"` crate dep (exact pin — see §7.3 caveat).
2. Implement `SandboxInvoker` against the `SandboxProvider` trait (§7.3).
3. Ship `MicrosandboxProvider` as the first concrete impl — thin wrapper over `microsandbox::Sandbox`.
4. Wire sandbox cache keyed by `(agent_instance_id, context_id, tool_id)` (§9.2); lazy first-use creation, context-scoped teardown. In-process reattach via `Sandbox::list()`/`get()` (hot reload, tests, liveness checks); no cross-process-restart reattach in v1 (§9.4).
5. Dispatch by `runtime.kind == sandbox`.
6. Keep first cut `Describe/Invoke` parity with process backend.
7. Tool-adapter startup pattern: first `exec_stream` on the sandbox's `tool-adapter` entrypoint, treat stdin/stdout as the TSRPC channel. (Alternative: container ENTRYPOINT auto-launch + socket — pick in implementation after a spike.)

### Phase D — Hardening

1. lifecycle events (sandbox create/start/stop/fail)
2. quarantine/backoff parity
3. idempotency-aware retry only
4. shutdown cleanup guarantees

### Phase E — DX + tooling

1. scaffold emits runtime block explicitly
2. optional sandbox runtime template
3. `check-external-tool` runtime/schema validator
4. docs for process vs sandbox tradeoffs
5. debug UX:
   - `cargo agent-platform tool-shell <tool> [--context <ctx>]` — attaches an interactive shell inside the running sandbox via `microsandbox::Sandbox::attach_shell()`
   - `cargo agent-platform tool-logs <tool> [--follow]` — tails sandbox logs / TSRPC channel events for the matching `baml:*` sandbox
   - `cargo agent-platform tool-inspect <tool>` — dumps sandbox spec, policy, lifecycle state

Activation model: **opt-in by metadata declaration** (`runtime.kind = sandbox`). No Cargo feature flag; matches how the current external-process backend is enabled (presence of metadata, not build-time gate). A runtime config flag can be added later if ops need a kill-switch without a rebuild.

### Phase F — MCP backend (placeholder)

`// TODO: to be implemented later`

**Role split (important):**
- **Default:** runtime is an **MCP client** connecting to an already-running MCP server managed outside our lifecycle (ops/user deploys + runs the server). Server lifecycle is **not our responsibility**.
- **Exception:** if we author and host an internal MCP server as an explicit platform feature, that is a separate work item with its own lifecycle story — not covered by this placeholder.

Scope (not designed here, reserved slot):
- dedicated `ToolBackend::Mcp` — adopt MCP-compliant external tool servers as a first-class tool source
- runtime connects over MCP transport (stdio attached to a pre-launched process, or HTTP/SSE to a remote endpoint)
- no spawn/teardown — only connect/disconnect. Think "database client," not "sandbox provider"
- catalog/policy/digest layering stays ours — MCP is only the invoker surface
- metadata shape for MCP tools is configuration-driven (endpoint URL, auth reference), not image-driven

Open questions to settle when starting this phase:
- metadata mapping: MCP `tools/list` responses → our `ExternalToolMetadata` (runtime-side adapter)
- auth/secret injection for client connections (header, bearer token, fnox-resolved credential)
- connection lifetime: per-context (reuse) vs per-invoke (reconnect each time)
- retry/reconnect semantics on MCP server restart
- handling server-declared tools that weren't in our deploy-time catalog (discovery vs strict allowlist)
- feature flag: `mcp-backend`

---

## 12) Test strategy

### Unit
- runtime parse/serialize
- resolver dispatch by runtime kind
- error classification parity across invokers

### Integration
- parameterize `Describe/Invoke` roundtrip fixtures over `ToolInvoker` impl
- same suite runs for `ProcessInvoker` and `MockSandboxInvoker`
- policy tests: timeout, egress denied, missing secret

### Regression
- external process fixtures unchanged and green
- static path unaffected
- metadata without `runtime` remains valid

---

## 13) Failure taxonomy

| Failure | Backend(s) | Classification target | Host retry policy |
|---|---|---|---|
| `ProcessSpawnFailed` | External | configuration / execution | no auto-retry |
| `DescribeProtocolMismatch` | External, Sandbox | configuration | no auto-retry |
| `InvokeTimeout` | External, Sandbox | transient | no auto-retry (idempotency unknown; caller may retry explicitly) |
| `PolicyDenied` | External, Sandbox | permission | no auto-retry |
| `ImagePullFailed` | Sandbox | configuration / transient | auto-retry with backoff (pull is idempotent) |
| `SandboxCreateFailed` | Sandbox | transient / execution | auto-retry with backoff (create is idempotent by name-replace) |
| `SandboxCreateTimeout` | Sandbox | transient (image pull or VM boot exceeded budget) | one auto-retry with longer timeout |
| `ResourceLimitExceeded` | Sandbox | execution | no auto-retry |
| `SandboxTerminatedUnexpectedly` | Sandbox | transient / execution | evict cache; next invoke cold-creates (implicit retry) |
| `SandboxTeardownFailed` | Sandbox | execution (best-effort cleanup; log + continue) | no retry |
| `SecretResolutionFailed` | External, Sandbox | configuration (Fnox miss, credential lookup error) | no auto-retry |
| `RpcChannelUnavailable` | External, Sandbox | transient (stdio closed, exec handle gone, reconnect needed) | reconnect + one auto-retry **if** operation declared idempotent |
| `PolicyCompilationFailed` | Sandbox | configuration (requested capabilities could not be mapped to `NetworkPolicy`) | no auto-retry |
| `CapacityExceeded` | External, Sandbox | permission (runner admission cap hit; see §10.3) | no auto-retry (operator must intervene) |
| `SetupFailed` | External | configuration (deploy-time setup command failed/timeout; see §4.2) | no auto-retry |

Mapping should continue to flow through existing `ClassifiedToolError` pathways.

---

## 14) Observability

### 14.1 Spans

Recommended span names:
- `tool.invoker.describe`
- `tool.invoker.invoke`
- `tool.sandbox.create`
- `tool.sandbox.teardown`

Attributes include:

Core:
- `tool.name`
- `tool.backend` (`static|external|sandbox|wasm`)
- `tool.digest`
- `tool.runtime_digest` (sandbox)

Correlation (critical for reattach/debug traces — without these, correlating a sandbox back to agent/context is painful):
- `sandbox.name` (the full `baml:<runner>:<agent>:<ctx>:<tool>` identifier)
- `runner_id`
- `agent_instance_id`
- `context_id`
- `tool_id`
- `task_id` (when available; varies per turn)

Align with existing `tool.*` span naming used by `ProcessToolHandler`; new sandbox spans extend the same namespace.

### 14.2 Metrics

Use common labels including `backend={static|external|sandbox|wasm}`:

| Metric | Type | Notes |
|---|---|---|
| `tool_describe_latency_ms` | histogram | millisecond buckets |
| `tool_invoke_latency_ms` | histogram | millisecond buckets |
| `tool_invocation_failures_total` | counter | labeled by failure class (§13) |
| `tool_sandbox_create_latency_ms` | histogram | cold-start cost |
| `tool_sandbox_active` | gauge | current warm count, labeled by agent_instance |

Naming convention: `_ms` suffix = histogram, `_total` = counter, gauges unsuffixed or `_active`/`_count`.

### 14.3 Lifecycle events

Standard event names:
- `sandbox.created`
- `sandbox.terminated`
- `sandbox.create_failed`
- `sandbox.policy_denied`

---

## 15) Platform constraints + rollout

Microsandbox's supported platforms constrain the sandbox backend:

1. **Linux with KVM enabled.** Primary target.
2. **macOS on Apple Silicon.** Supported dev platform.
3. **Intel Mac: not supported.** No KVM, no microsandbox.
4. **Windows: not supported.** Deferred indefinitely.
5. **Activation: opt-in by metadata declaration (`runtime.kind = sandbox`).** No build-time feature flag — matches the activation model of the current external-process backend. A runtime config kill-switch can be added later if operational need arises.
6. Start rollout on selected dev-agent-like tools; broader rollout gated by microsandbox beta stabilization.

Beta caveat: microsandbox explicitly warns "expect breaking changes." The `SandboxProvider` trait exists in part to contain that churn.

Debug note:
- Until Phase E debug UX lands, authors rely on sandbox logs/events for diagnosis.

---

## 16) Open questions

### Resolved (decisions folded into body)

- ~~Internal sandbox state reset between invokes~~ → **not runtime's concern**. Sandbox internals are a black box; author manages state inside the tool-adapter if needed. (§9)
- ~~Cross-tool sandbox sharing~~ → **one sandbox per tool per (agent_instance, context)**. No sharing. (§9.2)
- ~~Session-ID lifecycle / key~~ → **key = `(agent_instance_id, context_id, tool_id)`**, encoded as sandbox name `baml:<runner_id>:<agent_instance>:<context>:<tool>` (§9.2).
- ~~Idle/max timeout ownership~~ → **native microsandbox knobs** (`idle_timeout`, `max_duration`) wired through the provider. (§10.3)
- ~~`MultiSend` default for sandbox~~ → sandbox-per-`(agent_instance, context, tool)`. Strict overrides to per-invoke. (§9.5)
- ~~Transport for sandbox workload~~ → **TSRPC over `exec_stream` stdio** for v1 (microsandbox native, available today). The upcoming microsandbox Events API (`/run/agent.sock`) is a potential future alternative once GA. (§5.2, §11 Phase C)
- ~~Microsandbox integration shape~~ → **embedded Rust crate** (`cargo add microsandbox`), wrapped by `MicrosandboxProvider`. No daemon. (§7.3)
- ~~Crash/restart semantics~~ → **in-process reattach only in v1** (hot reload, tests, liveness checks). Cross-process-restart reattach explicitly out of scope: fresh `runner_id` per boot, cold cache on restart, microsandbox reaps orphans via `idle_timeout`. (§9.4)
- ~~Secret scoping / trust boundary~~ → runner resolves plaintext once (Fnox), hands to `.secret(...)` at create; microsandbox egress-injection keeps plaintext out of the VM for HTTP tools; TSRPC-carried per-invoke for non-HTTP fallback. (§10.1)
- ~~MCP in enum~~ → **not added until Phase F implementation starts.** Design-level placeholder only; no premature match churn. (§4.1, §11 Phase F)
- ~~Tool-adapter startup model inside the VM~~ → **v1: first `exec_stream` on the `tool-adapter` binary, TSRPC framed length-prefixed JSON over exec stdin/stdout** (§5.2). Future alternatives (kept as design escape hatches):
  - Container ENTRYPOINT auto-launches adapter that listens on a socket endpoint in-guest — adopt if microsandbox exposes port forwarding / vsock in a stable way.
  - Microsandbox Events API (`/run/agent.sock`) once GA — could carry TSRPC if shape permits.

### Still open

1. **Signing/attestation coupling** for sandbox images (Phase 2-lite deferred cosign; natural coupling with §8.4 digest pinning): same milestone or follow-up?
2. **Microsandbox Events API adoption**: when `/run/agent.sock` events GA, is it worth migrating TSRPC off exec stdio?
3. **Wasm backend timing**: visible now or feature-gated until active development?

---

## 17) Immediate lock decisions

1. Enum: `Static | External | Sandbox | Wasm`. **MCP not added to the enum yet** — reserved at design level only, variant lands when Phase F implementation starts (see #12).
2. Keep `External` = current process path.
3. Add optional `runtime` with `#[serde(default)]`.
4. Keep process wrapper default (`./tool-server`) in scaffolds; allow direct command override.
5. Sandbox workload uses TSRPC (`Describe/Invoke`), length-prefixed JSON framing (4-byte BE length + payload) over `exec_stream` stdio for v1. Logs on stderr, metrics host-side only (§5.3).
6. Require digest-pinned sandbox runtime refs and extend lock entry with runtime digest (`runtime_digest`).
7. Keep tool-sandbox and agent-sandbox protocols separate.
8. **Sandbox lifetime keyed by `(agent_instance_id, context_id, tool_id)`**, encoded as microsandbox sandbox name `baml:<runner_id>:<agent_instance>:<context>:<tool>`. `runner_id` is a UUID generated at runner boot (in-memory only). Created lazily on first invoke; torn down on context close, `idle_timeout`, or `max_duration`. Reattach validates status + context liveness + runtime digest + policy hash + age (§9.4).
9. **One sandbox per tool per (agent_instance, context)** — no cross-tool or cross-instance sharing.
10. **`SandboxProvider` trait is the runtime's only entrypoint to sandbox infrastructure.** First impl: `MicrosandboxProvider`, thin wrapper over the **embedded** `microsandbox` crate. No external daemon.
11. **Sandbox internal state is a black box to the runtime.** Any reset/cleanup between invokes is the tool-adapter's responsibility.
12. **MCP placeholder**: reserved in design only (§11 Phase F). Not added to `ToolBackend` enum until implementation starts, to avoid premature exhaustive-match churn. When added, runtime role will be **client only** (connect to externally-managed MCP servers).
13. **Secrets via microsandbox placeholder-substitution for HTTP tools** (native egress injection; plaintext never enters VM). Runner resolves plaintext via Fnox, hands to `.secret(...)` at `create()`. Non-HTTP auth fallback: TSRPC-carried per invoke.
14. **Reattach is in-process only in v1** (hot reload, tests, liveness checks) via `Sandbox::list()` / `get(name)`. Cross-process-restart reattach is explicitly out of scope: fresh `runner_id` UUID per boot, restarted runner starts with cold cache, microsandbox reaps previous runner's orphans via `idle_timeout`. Follow-up if needed.
15. **Setup hooks apply to `process` kind only.** Sandbox kind uses the pre-built image as the immutable artifact; digest scope = artifact dir only (process kind) or image layers (sandbox kind).
16. **Strict session policy overrides cache** — `ExternalSessionPolicy::Strict` forces sandbox-per-invoke regardless of the `(agent_instance, context, tool)` cache.
17. **Capacity admission: `FailFast` only in v1.** Named alternatives (`EvictIdle`, `Queue`) reserved for post-v1 (§10.3).
18. **Secret bindings are mode-exclusive per binding** — create-time egress-bound OR per-invoke TSRPC-carried, never both in v1 (§10.1a).

This preserves current working behavior and provides a clear path to microsandbox-backed isolation.

---

## 18) Development split (implementation workstreams)

To execute this safely, split implementation into parallel workstreams with clear handoff points.

### Workstream A — Core runtime abstractions (owner: runtime/tools) — ✅ COMPLETED (2026-04-17)

**Scope**
- Add `ToolBackend::Sandbox`.
- Introduce `ToolRuntime` + metadata parsing defaults.
- Introduce `ToolInvoker` abstraction and keep process path behavior identical.

**Deliverables**
- Compile-clean refactor with no behavior changes.
- Existing external-process fixtures unchanged and green.

**Depends on**
- none (first stream).

**Completion notes**
- `ToolBackend::Sandbox` added at `crates/baml-rt-tools/src/tools.rs:1263` (no dispatch yet — B).
- New module `crates/baml-rt-tools/src/external_tools/runtime.rs` with `ToolRuntime` (tagged `kind: process|sandbox`), `ProcessRuntimeSpec`, `SandboxRuntimeSpec`, `ToolRuntimeKind`, `DEFAULT_PROCESS_COMMAND`. Missing `runtime` → process wrapper default (§4.2 rule 2).
- `ExternalToolMetadata.runtime: Option<ToolRuntime>` added (`#[serde(default)]`, `skip_serializing_if = Option::is_none`) in `external_tools/metadata.rs`; back-compat preserved (all existing `tool-metadata.json` files parse unchanged).
- `ToolInvoker` trait added in `external_tools/invoker.rs` alongside `ExternalInvoker`, with blanket impl `impl<T: ExternalInvoker + ?Sized> ToolInvoker for T` — zero-refactor hook for `SandboxInvoker` / `WasmInvoker` in later workstreams.
- Re-exports wired in `external_tools/mod.rs` (`ToolInvoker`, runtime types).
- Verification: `cargo build -p baml-rt-tools` green; `cargo test -p baml-rt-tools` green (134 + aux suites, 4 new runtime roundtrip tests); `cargo clippy -p baml-rt-tools --all-targets -- -D warnings` clean.
- **Not in scope (deferred):** schema JSON `runtime` property + `if/then` digest rule → C; dispatch by runtime kind + `microsandbox` dep + `SandboxProvider` → B.

---

### Workstream B — Microsandbox provider integration (owner: runtime/sandbox) — ✅ COMPLETED (2026-04-17)

**Scope**
- Add `microsandbox = "=0.3.13"` dep (exact pin; beta crate, bumps go through explicit review).
- Add `SandboxProvider` trait + `MicrosandboxProvider` impl.
- Implement v1 channel over `exec_stream` (length-prefixed frames).
- Implement sandbox cache keyed by `(agent_instance_id, context_id, tool_id)` + `runner_id`-prefixed names.
- Implement in-process reattach + validation checklist.

**Deliverables**
- `SandboxInvoker` functional parity for `Describe/Invoke`.
- Opt-in by metadata declaration (`runtime.kind = sandbox`); no Cargo feature flag, matching current external-process activation model.

**Depends on**
- Workstream A abstractions.

**Completion notes**
- New submodule `crates/baml-rt-tools/src/external_tools/sandbox/` with:
  - `spec.rs` — `SandboxSpec`, `SandboxHandle` (with `runtime_digest`, `policy_hash`, `max_duration` for reattach checklist), `SandboxEvent`, `NetworkPolicy`, `SecretBinding{Mode::EgressBound|PerInvoke}`, `ImageDigest`, `PullPolicy`, `VolumeMount`, `PortMapping` (§7.3 shape).
  - `provider.rs` — `SandboxProvider` trait (`create` / `rpc_channel` / `teardown` / `events` / `list_owned` / `reattach`) per §7.3.
  - `channel.rs` — `TsrpcChannel` length-prefixed JSON codec (4-byte BE len + payload, 64 MiB ceiling) with `send`/`recv` over any `AsyncRead + AsyncWrite` (§5.2).
  - `microsandbox_provider.rs` — `MicrosandboxProvider` gated behind the new `sandbox-provider` Cargo feature (beta-containment; §7.3 caveat). Scaffolded method bodies document the SandboxBuilder mapping; the 0.3.13 API surface was probed (`Sandbox::builder` / `image` / `cpus` / `memory` / `secret_env` / `network` / `create_detached` / `exec_stream` / `stop_and_wait` / `kill` / `Sandbox::start` for reattach). Full API wiring deferred behind the feature to avoid build breakage while the crate churns; the `SandboxProvider` trait isolates the runtime from it.
  - `mock.rs` — `MockSandboxProvider` + `ScriptedAdapter` for tests + fixture-driven dev runs (in-memory duplex pair; no VM).
  - `invoker.rs` — `SandboxCache` keyed by `(agent_id, context_id, tool_name)` encoded as `baml:<runner_id>:<agent>:<ctx>:<tool>` (§9.2); lazy first-use `get_or_create`; reattach validation checklist (age + runtime-digest + policy-hash match; context/status checks deferred to upstream). `SandboxInvoker: ToolInvoker` wraps provider + cache and frames JSON-RPC 2.0 `tool/describe` / `tool/invoke` over `TsrpcChannel`; error responses flow through the existing `map_jsonrpc_error` so the §13 failure taxonomy is shared with the process backend.
  - `handler.rs` — `SandboxToolHandler: ToolHandler` constructs per-session `SandboxInvoker` using `ToolSessionContext.{agent_id, context_id}`, mirrors `ProcessToolHandler` FSM semantics (`Open`/`Send`/`Read`/`Finish`/`Abort` with quarantine + backoff).
- `Cargo.toml`: added `microsandbox = { version = "=0.3.13", optional = true }` + `futures-util` (workspace) dep; new `sandbox-provider` feature.
- `DevModeResolver::from_dirs_with_sandbox` branch: when metadata declares `runtime.kind = "sandbox"`, routes through `SandboxToolHandler` via a runner-provided `SandboxRuntimeWiring { provider, cache, spec_factory }`; missing wiring + sandbox metadata → hard error at load time. Process path unchanged.
- **Activation model preserved (§15):** metadata-declaration is the trigger. The `sandbox-provider` Cargo feature exists purely as a beta-containment gate; the trait, cache, handler, and mock provider all compile without it.
- **Deliberate deferrals inside B:** (a) full `MicrosandboxProvider` create/rpc/teardown wiring (feature-gated scaffold today — crate API churn); (b) `Sandbox::list()` primitive not in 0.3.13 → `list_owned` returns empty; (c) describe() is lazy (first-invoke) for sandbox tools — contract validation reuses the existing `tool/describe` path per-context; (d) context-liveness + status reattach checks belong upstream (need active-context registry), handle-side age + digest + policy checks are in.
- Verification: `cargo build -p baml-rt-tools` green; `cargo test -p baml-rt-tools --lib external_tools::sandbox` green (3 tests — channel roundtrip, oversized-frame reject, full invoker happy-path through mock provider with JSON-RPC echo adapter); full `cargo test -p baml-rt-tools` green (139 unit + aux); `cargo clippy -p baml-rt-tools --all-targets -- -D warnings` clean.
- **Not in scope (follow-up workstreams):** Workstream D — secret-binding mode enforcement, `NetworkPolicy` compilation from declared capabilities, `FailFast` admission caps, setup-hook guardrails. Workstream E — lifecycle spans/metrics on the sandbox path, parameterized integration suite, correlation attrs. Workstream F — operator runbook + rollout gating. Microsandbox API full binding once the crate stabilizes past beta.

---

### Workstream C — Metadata/schema/CLI surface (owner: builder/cli) — ✅ COMPLETED (2026-04-17)

**Scope**
- Extend metadata schema with `runtime` (backward compatible).
- Add schema `if/then` requirement for sandbox `runtime_digest`.
- Scaffold/runtime-validator updates (`check-external-tool`).
- Keep wrapper default for process scaffolds.

**Deliverables**
- Schema + parser compatibility tests.
- CLI emits valid process/sandbox metadata blocks.

**Depends on**
- Workstream A (types), partial B (runtime fields validation expectations).

**Completion notes**
- Schema extended at `schemas/external_tool_metadata.schema.json` with optional `runtime` and conditional `runtime_digest` requirement when `runtime.kind == sandbox`.
- `ExternalToolMetadata` extended with `runtime_digest: Option<String>` in `crates/baml-rt-tools/src/external_tools/metadata.rs` (backward-compatible parse preserved).
- Scaffolder now emits explicit runtime blocks in `crates/cargo-agent-platform/src/templates/external_tool/metadata_json.rs`: process by default (`runtime: { kind: "process", ... }`) and sandbox via `cargo agent-platform new-tool --runtime sandbox --sandbox-image ... --runtime-digest ...` (optional `--sandbox-entrypoint ...`).
- New validator command added: `cargo agent-platform check-external-tool --path <dir>` (`crates/cargo-agent-platform/src/commands/check_external_tool.rs`, wired in `main.rs` and `commands/mod.rs`).
- Docs updated in `docs/sdk-cli.md` with command reference for `check-external-tool`.
- Verification: targeted tests green — `cargo test -p baml-rt-tools metadata_deserializes -- --nocapture` and `cargo test -p cargo-agent-platform scaffolded_metadata -- --nocapture`.

---

### Workstream D — Security/policy plumbing (owner: platform/security)

**Scope**
- Secret binding modes (`create-time egress-bound` vs `per-invoke`), mode exclusivity enforcement.
- `NetworkPolicy` compilation from requested + authorized capabilities.
- Capacity admission mode (`FailFast` in v1).
- Setup hook guardrails for process kind (timeouts/output limits/error mapping).

**Deliverables**
- Policy compiler + classifier integration.
- Deterministic failure mapping (`PolicyCompilationFailed`, `CapacityExceeded`, `SetupFailed`, etc.).

**Depends on**
- A + B + C.

---

### Workstream E — Observability + test matrix (owner: observability/qa)

**Scope**
- Spans/metrics/lifecycle events for sandbox path.
- Parameterized `ToolInvoker` integration suite (Process + MockSandbox + Microsandbox when available).
- Regression gates (static/process paths unaffected).

**Deliverables**
- CI-ready suite for Gate A/B/C.
- Traceability via correlation attrs (`runner_id`, `agent_instance_id`, `context_id`, `tool_id`, `sandbox.name`).

**Depends on**
- A + B (+ D for policy/failure assertions).

---

### Workstream F — Rollout + operations (owner: runtime/ops)

**Scope**
- Activation model: opt-in by metadata declaration (`runtime.kind = sandbox`). No Cargo feature flag.
- Environment qualification (Linux KVM + Apple Silicon dev support).
- Operator runbook: capacity caps, idle/max duration defaults, forced sandbox recycle for credential rotation.

**Deliverables**
- Staged rollout checklist matching gates A→E (start with a small set of sandbox-declared tools; broaden as confidence grows).
- **No rollback path** in v1: sandbox-kind tools declare sandbox as their only runtime. Disabling sandbox = those tools become unavailable (not demoted to process). If the capability matters later, a runtime kill-switch + per-tool fallback is a follow-up design.

**Depends on**
- A–E completed for production rollout.

---

### Suggested execution order (high level)

1. **A** (abstractions, no behavior change)
2. **B + C** in parallel
3. **D** once B/C interfaces are stable
4. **E** throughout, finalize after D
5. **F** after Gate C confidence, full after Gate E

This split keeps risk localized and allows parallel progress without destabilizing the current process backend.