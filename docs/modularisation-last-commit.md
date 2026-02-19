# Modularisation: Last Commit (9f0a31f)

Large production files in the commit and proposed splits, with pub interface control.

---

## 1. `crates/baml-rt-tools/src/tools.rs` (~1900 lines)

**Current:** Single module with types, registry, session handle, BamlTool path, TypedToolFunction path, create_multi_send_session_tool_from_async, and internal session impls (OneShotSession, MultiSendSession, etc.).

**Proposed split:**

| New module | Contents | Pub at crate | Notes |
|------------|----------|--------------|--------|
| `tools/types.rs` | ToolAccess, ToolSecretRequirement, ToolTypeSpec, BundleName, LocalToolName, ToolName, ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder, ToolFunctionMetadataExport, ToolDiscoveryRecord, ToolBundleMetadata, ToolCapability, ToolOrigin, ToolSessionContext, ToolHandler, ToolBundle | Re-export from `tools` only; no new crate-level pub | Types and traits used by registry/handles |
| `tools/helpers.rs` | capitalize_first, empty_open_input, deserialize_tool_input, serialize_tool_output, validate_open_input, parse_tool_name_and_class, map_session_error, tool_registry_trace* | `pub(crate)` or private | Shared helpers |
| `tools/registry.rs` | ToolRegistryInner, ToolRegistry, impl ToolRegistry | ToolRegistry already pub | Bulk of registry logic |
| `tools/session_handle.rs` | AwaitingInput, Ready, Closed, SessionState, ToolSessionAdvance, ToolSessionHandle + impls | Re-export from `tools` only | Session FSM handle |
| `tools/executor.rs` | ToolExecutor, ExecutorAdapter | ToolExecutor pub; ExecutorAdapter private | Execution abstraction |
| `tools/baml_tool.rs` | ToolWrapper, create_tool_handler, OneShotSession + impls | create_tool_handler pub; OneShotSession/ToolWrapper private | BamlTool → one-shot session |
| `tools/async_session.rs` | create_multi_send_session_tool_from_async, MultiSendSessionToolFromAsync, MultiSendSession + impls | create_multi_send_session_tool_from_async pub; MultiSendSessionToolFromAsync, MultiSendSession private | Async fn → multi-send session |
| `tools/typed_function.rs` | TypedToolFunction + impls | TypedToolFunction pub | TypedToolFunction (open_input=()) |
| `tools/mod.rs` | Re-exports from submodules; BamlTool trait (or move to types) | Same as current lib.rs `pub use tools::...` | Single pub surface for `tools` |

**Pub interface control:** Keep current `lib.rs` re-exports unchanged. New modules use `pub(crate)` or `pub(super)` where possible; only the same names as today are `pub` at the crate boundary.

---

## 2. `crates/baml-rt-quickjs/src/baml.rs` (~1424 lines)

**Current:** BamlRuntimeManager, ToolExecutionHandle, ToolSessionExecutionHandle, load_schema, invoke_function, tool session open/send/next/finish/abort, trait impls (BamlFunctionExecutor, SchemaLoader, Default). Already has subdir `baml/` with `tool_extraction.rs`.

**Proposed split:**

| New module | Contents | Pub at crate | Notes |
|------------|----------|--------------|--------|
| `baml/manager.rs` | BamlRuntimeManager struct, new(), set_effect_emitter, set_conversation_context_provider, set_parse_retry_policy, tool_execution_handle(), tool_session_handle(), is_schema_loaded(), load_schema() | BamlRuntimeManager, load_schema, is_schema_loaded, tool_session_handle | Core manager API |
| `baml/execution_handle.rs` | ToolExecutionHandle, ToolCallSessionState, ToolSessionScope + impl | pub(crate) or pub if used by bridge | Direct tool execution path |
| `baml/session_handle.rs` | ToolSessionExecutionHandle + impl (open_session, send, next, finish, abort) | ToolSessionExecutionHandle pub | Session lifecycle |
| `baml/invoke.rs` | invoke_function and the rest of BamlRuntimeManager execution logic | pub(crate) | Large block of invocation logic |
| `baml/traits_impl.rs` | impl BamlFunctionExecutor, SchemaLoader, Default for BamlRuntimeManager | — | Trait impls |
| `baml.rs` | Re-exports BamlRuntimeManager, ToolSessionExecutionHandle | Same as now | Thin facade |

**Pub interface control:** Only `BamlRuntimeManager` and `ToolSessionExecutionHandle` stay pub at quickjs crate level; internal helpers and execution_handle stay `pub(crate)`.

---

## 3. `crates/baml-rt-quickjs/src/quickjs_bridge.rs` (~1343 lines)

**Current:** QuickJSBridge struct + one large impl block. Already has subdir `quickjs_bridge/` (eval, js_codegen, promise_polling, scope, stream, stream_yield, tools, wrappers).

**Proposed:** Move the remaining impl body from `quickjs_bridge.rs` into `quickjs_bridge/impl.rs` or split by concern (tools registration, schema loading, a2a handling, etc.) so the root file is a thin facade and pub interface is explicit. Keep `QuickJSBridge` and its public methods as the only pub API; internal helpers in submodules with `pub(crate)`.

---

## 4. `crates/baml-rt-a2a/src/a2a_transport.rs` (~1126 lines)

**Current:** TaskStoreConversationContextProvider, A2aAgent, LiveStreamSession, A2aAgentBuilder, A2aAgentBuilderWithEffectEmitter, RuntimeConfig/BridgeConfig/TaskStoreConfig/ProvenanceWriterConfig/AgentIdConfig, JsToolHandler, JsToolSession, impl A2aRequestHandler, impl A2aAgent (large).

**Proposed split:**

| New module | Contents | Pub at crate | Notes |
|------------|----------|--------------|--------|
| `a2a_transport/context_provider.rs` | TaskStoreConversationContextProvider | private | Conversation context |
| `a2a_transport/agent.rs` | A2aAgent struct, LiveStreamSession, core impl A2aAgent | A2aAgent pub | Agent type and core methods |
| `a2a_transport/builder.rs` | A2aAgentBuilder, A2aAgentBuilderWithEffectEmitter, *Config enums, impls | Builder types pub | Builder pattern |
| `a2a_transport/request_handler.rs` | impl A2aRequestHandler for A2aAgent | — | Trait impl |
| `a2a_transport/js_tool.rs` | JsToolHandler, JsToolSession + impl ToolHandler, ToolSession | private | JS tool adapter |
| `a2a_transport/mod.rs` | Re-exports A2aAgent, builder types | Same as now | Single pub surface |

**Pub interface control:** A2aAgent and builder types stay pub; config enums and JS tool adapter stay `pub(crate)` or private.

---

## 5. `crates/baml-agent-runner/src/main.rs` (~915 lines)

**Current:** AgentPackage, BootedAgent, AgentRunner, RunnerRegistry, RunnerConfig, Cli, helpers (strip_stream_suffix, split_agent_method, is_a2a_method, map_a2a_error, stdio_context_id, stdio_task_id, wrap_plaintext_message), build_provenance_writer, main().

**Proposed split:**

| New module | Contents | Pub | Notes |
|------------|----------|-----|--------|
| `main.rs` | Cli, main(), build_provenance_writer | — | Entrypoint only |
| `agent_package.rs` or `package.rs` | AgentPackage | pub(crate) | Package loading |
| `booted_agent.rs` | BootedAgent | pub(crate) | Post-boot state |
| `runner.rs` | AgentRunner, impl | pub(crate) | Runner logic |
| `registry.rs` | RunnerRegistry, impl AgentLister, AgentRegistry, A2aRequestHandler | pub(crate) | Registry impl |
| `a2a_helpers.rs` or `http.rs` | strip_stream_suffix, split_agent_method, is_a2a_method, map_a2a_error, stdio_* | private | A2A/HTTP helpers |
| `message.rs` | wrap_plaintext_message | private | Message formatting |

**Pub interface control:** Binary crate; no public API. All new modules are `pub(crate)` or private; only `main` is the entrypoint.

---

## 6. Other large files (shorter recommendations)

- **`crates/baml-rt-tools/src/notion.rs`** (~969 lines): Consider splitting into `notion/types.rs` and `notion/impl.rs` or by tool (search, get_page, get_blocks) with a single pub module.
- **`crates/baml-rt-tools/src/clickup.rs`** (~651 lines): Same idea: types vs impl, or one module per “tool” if multiple.
- **`crates/baml-rt-builder/src/baml-agent-builder.rs`** (~506 lines): Extract “build steps” or “artefact writing” into builder/ submodules with a small pub API.

---

## Implementation order

1. **baml-rt-tools/src/tools.rs** – Highest impact (single ~1900-line file). Split into `tools/{types,helpers,registry,session_handle,executor,baml_tool,session_tool,typed_function,mod}.rs` and keep current `lib.rs` re-exports.
2. **baml-rt-quickjs/src/baml.rs** – Split into `baml/{manager,execution_handle,session_handle,invoke,traits_impl}.rs` + `baml.rs` facade.
3. **baml-rt-a2a/src/a2a_transport.rs** – Split into `a2a_transport/{context_provider,agent,builder,request_handler,js_tool,mod}.rs`.
4. **baml-agent-runner/src/main.rs** – Split into runner, registry, package, booted_agent, helpers, main.
5. **quickjs_bridge.rs** – Move impl into quickjs_bridge/ and keep a thin root.
6. **notion/clickup/baml-agent-builder** – Optional; split when touching those areas.

All new modules should minimise `pub` to the crate boundary; use `pub(crate)` or `pub(super)` and only re-export what the crate’s public API already exposes.
