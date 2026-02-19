# Rust Interface Architect — Tool Abstractions Review

*Invocation: verify tool abstractions conform to type-system discipline, boundaries are clear, and types are composable for building many tools.*

---

## 1. Current state (public surfaces)

### 1.1 Boundary types (newtypes / DUs)

| Type | Role | Assessment |
|------|------|------------|
| `ToolName` | Qualified name `bundle/local`; parse-only construction | ✓ Newtype; invalid names fail at parse. |
| `BundleName`, `LocalToolName` | Sub-components; validated in `new()` / `parse()` | ✓ No raw strings at registry boundary. |
| `ToolSessionId` | UUID wrapper; `parse()` / `random()` | ✓ Newtype. |
| `ToolCapability` | `OneShot \| Streaming` | ✓ DU; no bare bool. |
| `ToolOrigin` | `Host \| Guest` | ✓ DU. |
| `ToolAccess` | `Read \| Write \| Delete` | ✓ DU. |
| `ToolStep` | `Streaming { output } \| Done { output } \| Error { error }` | ✓ DU; session step is explicit. |
| `ToolSessionAdvance` | Carries `ToolSessionHandle<Ready>` or `ToolSessionHandle<Closed>` | ✓ State encoded in handle phantom. |
| `ToolSessionHandle<State: SessionState>` | Phantom state: `AwaitingInput`, `Ready`, `Closed` | ✓ Invalid transitions unrepresentable at type level. |

### 1.2 Traits (capability-oriented vs god traits)

| Trait | Purpose | Object-safe? | Assessment |
|-------|---------|--------------|------------|
| `ToolHandler` | Metadata + capability + open_session → Box<dyn ToolSession> | Yes (async_trait) | ✓ Narrow: “something the registry can run.” |
| `ToolSession` | send / next / finish / abort | Yes | ✓ Session protocol only. |
| `ToolExecutor` | execute(Value) → Result<Value> (internal bridge) | Yes | ✓ Internal; bridges closures to async trait. |
| `ToolBundle` | metadata + functions() → Vec<Arc<dyn ToolHandler>> | Yes | ✓ Bundle = collection of handlers. |
| `BamlTool` | Type-level name, OpenInput/Input/Output, execute | No (associated types, const) | ✓ Used generically by create_tool_handler; not dyn. |
| `ToolMetadataBuilder` | build_metadata() | No | ✓ Builder; single method. |
| `SessionState` | IS_CLOSED const | No (marker) | ✓ Phantom for handle states. |

**Verdict:** Traits are narrow and capability-oriented. Object-safety is used only where erasure is required (registry, bundle, session).

### 1.3 Handler construction paths (composability)

All paths produce `Arc<dyn ToolHandler>` and compose in `ToolBundle::functions()`:

| Path | Inputs | Output | Use case |
|------|--------|--------|----------|
| `create_tool_handler<T: BamlTool>(tool: T)` | Typed tool impl | (ToolFunctionMetadata, Arc<dyn ToolHandler>) | Inventory tools with type-level metadata. |
| `create_multi_send_session_tool_from_async<OI,I,O,F>(metadata, executor)` | Metadata + Fn(I)→Future<Output=Result<O>> | Arc<dyn ToolHandler> | Multi send/next from async fn; arbitrary OI. |
| Hand-rolled `impl ToolHandler` | e.g. A2aSessionToolHandler | Arc<dyn ToolHandler> | Custom session semantics (streaming, external protocol). |

**Composability:** Bundles can mix any of the above. Example: `SystemBundle` = A2aSessionBundle (hand-rolled ToolHandler) + DiscoverBundle (create_multi_send_session_tool_from_async). Same `Vec<Arc<dyn ToolHandler>>`; no adapter glue.

### 1.4 Type consistency across paths

- **OpenInput / Send input / Output:** BamlTool uses associated types; create_multi_send_session_tool_from_async uses type parameters `<OI, I, O>`. All flow into `ToolFunctionMetadata` (open_input_schema, input_schema, output_schema) and into the same `ToolSession` protocol (Value in/out). So: metadata and runtime are aligned; tool authors choose either “trait + associated types” or “metadata + async fn” and get the same registry/session machinery.
- **ToolType bound:** Used for BamlTool and for OI/I/O in async factories (JsonSchema + TS + Send + Sync + 'static). Single place for “schema + TS” requirement; no drift.

---

## 2. Findings (boundaries and tension)

### 2.1 Optionality in metadata (architect preference: label or eliminate)

- **ToolFunctionMetadata:** `baml_decl: Option<String>`, `access: Option<ToolAccess>`. Meaning: “optional BAML override,” “optional access level.” Acceptable; if we ever want “no implicit default,” these could become DUs (e.g. `BamlDecl::FromSchema | Override(String)`, `AccessRequirement::Any | Require(ToolAccess)`). **Recommendation:** document current semantics; consider DUs only if product rules tighten.
- **ToolBundleMetadata:** `config_schema: Option<Value>`. Same: optional bundle config. Document; consider DU only if “no config” must be explicit.

### 2.2 ToolCapability default and async-from-async handlers

- **ToolHandler::capability()** defaults to `ToolCapability::OneShot`. All current async-from-async handlers (MultiSendSessionToolFromAsync) use that default. Multi-send factory = one open, many send/next, reported as OneShot.
- **Semantics:** OneShot = “registry.execute() is allowed (one open/send/next).” Streaming = “must use open_session; execute() returns error.” Discover tools are valid for a single execute() call (one page of results). So default OneShot for both async factories is correct. No change needed.

### 2.3 Async factory vs hand-rolled ToolHandler

- **create_multi_send_session_tool_from_async:** No struct; closure captures deps; open validates OI; send runs fn(I)→O. Discovery uses this.
- **Hand-rolled impl ToolHandler:** Full control; e.g. A2a uses custom ToolHandler (streaming, external A2aRequestHandler). Solid split. So: “stateful trait impl” vs “stateless closure + metadata.” Both produce the same ToolHandler; no overlap in responsibility. Discovery correctly uses the async factory; A2a uses hand-rolled ToolHandler (streaming, external A2aRequestHandler). Solid.

### 2.4 ToolExecutor and ExecutorAdapter (internal seam)

- **ToolExecutor:** async fn execute(Value)→Result<Value>. Used only inside session implementations (OneShotSession, MultiSendSession). Not public API for tool authors.
- **ExecutorAdapter:** wraps Fn(Value)→Pin<Box<dyn Future<Output=Result<Value>> + Send>> to implement ToolExecutor. Keeps “deserialize I → run future → serialize O” inside the handler; tool author only supplies typed I/O. Boundary is clear; no leak.

### 2.5 Visibility

- **Public:** ToolHandler, ToolSession, ToolBundle, BamlTool, ToolName, ToolFunctionMetadata, ToolRegistry, ToolSessionHandle, ToolStep, ToolCapability, ToolOrigin, create_* functions, validate_open_input, parse_tool_name_and_class. Needed for registry, bundles, and tool authors.
- **pub(crate):** capitalize_first, ToolWrapper. Correct; not part of stable API.
- **Internal (no pub):** ToolRegistryInner, ExecutorAdapter, MultiSendSessionToolFromAsync, OneShotSession, MultiSendSession, etc. Good; minimal public surface.

---

## 2.6 Session protocol and ToolCapability (deeper analysis)

**Protocol (ToolSession):** `send(input)` then `next()` — possibly multiple `next()` calls. Each `next()` returns a `ToolStep`:

- **ToolStep::Streaming { output }** — a chunk; more steps may follow. `next()` may block or buffer until more data arrives. Completion is *not* indicated; caller must call `next()` again.
- **ToolStep::Done { output }** — execution for this response is complete (optional final payload). No further `next()` needed for this send.
- **ToolStep::Error { error }** — failure; session typically closed.

So the *trait difference* is: does this tool ever emit `Streaming`, or only `Done`/`Error`?

| Session implementation | send/next pattern | Step shape | capability() |
|------------------------|--------------------|------------|--------------|
| OneShotSession | one send, one next; next() runs work and returns | Always `Done` (or `Error`) | OneShot |
| MultiSendSession | send runs work, next() drains pending; repeatable send/next | Always `Done` (or `Error`) per round | OneShot |
| A2aSession | one send starts stream; next() returns `Streaming` N times (blocking on channel), then `Done` | `Streaming`… then `Done` | Streaming |

**Boundary:** For **OneShot** tools, a single `next()` after a `send()` returns the full result and signals completion (step is `Done`). For **Streaming** tools, `next()` may be called multiple times; each `next()` may block or buffer and does *not* by itself indicate completion; the caller must keep calling `next()` until the step is `Done`. So `ToolCapability` is the handler’s declaration: “I never emit `Streaming`” (OneShot) vs “I may emit `Streaming`” (Streaming). That is why `ToolRegistry::execute()` (which does one open → send → next → finish) is only allowed when capability is OneShot: if the tool were Streaming, that single `next()` might return `Streaming` and the caller would treat it as complete and finish the session, losing the rest of the stream.

**Optionality / DU:** The protocol already encodes completion via the step variant (`Done` vs `Streaming`). No optionality blur; the capability is a static declaration on the handler, not per-step.

**Proposed rustdoc alignment:** Document `ToolCapability` in terms of `ToolStep`: OneShot = this tool only ever returns `Done` (or `Error`) from `next()`, so one `next()` indicates completion. Streaming = this tool may return `ToolStep::Streaming`; `next()` may block/buffer and does not indicate completion until the step is `Done`.

---

## 3. Conformance checklist (architect tenets)

| Tenet | Status |
|-------|--------|
| Newtype / DU for boundary and state | ✓ ToolName, BundleName, ToolSessionId, ToolCapability, ToolOrigin, ToolStep, ToolSessionHandle<State>. |
| Optionality labelled or eliminated | ⚠ Metadata has Option<>; documented; DUs optional later. |
| Boundary seams clear (I/O vs domain, sync vs async) | ✓ ToolHandler/ToolSession are async; ToolExecutor bridges; Value at session boundary. |
| Narrow, capability-oriented traits | ✓ ToolHandler, ToolSession, ToolBundle, ToolExecutor; BamlTool is generic-only. |
| Object-safe only where erasure needed | ✓ dyn ToolHandler, dyn ToolSession, dyn ToolExecutor; BamlTool not object-safe. |
| Visibility minimized | ✓ Only registration/session types and factories are pub. |
| Invalid state unrepresentable | ✓ ToolSessionHandle<State>; ToolStep DU; ToolCapability DU. |

---

## 4. Composability for “building a lot of tools”

- **Three construction paths** (BamlTool, create_multi_send_session_tool_from_async, hand-rolled ToolHandler) cover: static tools, session tools from async fn + metadata, and custom protocols (e.g. A2a streaming).
- **Single erasure point:** `Arc<dyn ToolHandler>`. Bundles return `Vec<Arc<dyn ToolHandler>>`; registry stores the same. All tools participate in the same open_session/send/next and (when capability is OneShot) execute() flow.
- **Metadata:** ToolFunctionMetadata is the single descriptor (name, schemas, tags, origin, etc.). Built from types (TypeBasedMetadataBuilder) or by hand (e.g. system_discover_agents_metadata()). Composable: add tools by adding handlers; add handlers by any of the three paths or custom impl.

---

## 5. Optional refinements (no implementation unless sanctioned)

1. **ToolFunctionMetadata Option → DU (low priority):** If product requires “no implicit default” for access or BAML decl, introduce small DUs and document migration for existing call sites.
2. **ToolBundleMetadata.config_schema:** Same; consider DU only if “no config” must be explicit.
3. **Document ToolCapability in rustdoc:** Clarify that OneShot = “execute() allowed (one round)”; Streaming = “session only; may emit multiple Streaming steps.” Helps future tool authors choose capability when implementing ToolHandler manually.

---

## 6. Risks

- **Adding a new construction path:** If a fourth pattern appears (e.g. “streaming generator”), prefer extending an existing path (e.g. a variant of the async factory or a small trait) rather than a new god factory. Current set is already complete for static, session-one-shot, session-multi-send, and custom.
- **Changing ToolHandler (e.g. new method):** All handlers (BamlTool wrapper, async-from-async, A2a) must be updated; default implementations (e.g. capability()) reduce blast radius.

---

## 7. Summary

- **Conformance:** Abstractions conform to the architect’s tenets: newtypes/DUs at boundaries, narrow traits, object-safety only where needed, clear seams, minimal visibility.
- **Composability:** All tools erase to `Arc<dyn ToolHandler>`; bundles and registry are agnostic to construction path. Types are composable for building many tools.
- **No unused abstractions:** BamlTool, create_multi_send_session_tool_from_async, and hand-rolled ToolHandler each have a clear role; discovery uses the multi-send async factory; A2a uses custom ToolHandler; inventory tools use BamlTool.

*End of review. Implement optional refinements only if the Fabricator sanctions.*
