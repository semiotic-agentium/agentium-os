# If You Only Read One File in Agentium OS, Read `baml.rs`

Most systems have a file where architecture becomes real. In this codebase, that file is:

`crates/baml-rt-quickjs/src/baml.rs`

If you are onboarding, debugging, or designing new agent behavior, this file gives you the most leverage per minute.

## Why This File Is the Priority

`baml.rs` is the runtime control plane for agent execution. It connects:

- BAML function invocation
- Tool resolution and execution
- Session-plan interpretation
- Context propagation and observability hooks

In other words: it is where "prompt output" becomes constrained, executable behavior.

## What Engineers Should Appreciate

## 1) Deterministic tool binding over free-form tool choice

The runtime does not trust model output to name tools directly for session plans. It resolves tools from builder artifacts (`session_plan_functions.json`) and registered metadata.

That design is easy to miss, and it is one of the most important safety properties in the project.

```rust
if let Some(plan) = extract_tool_session_plan(&baml_result)? {
    let tool_name = if let (Some(func_name), Some(map)) =
        (source_baml_function, &self.session_plan_functions)
    {
        if let Some(plan_type) = map.get(func_name) {
            resolve_tool_name_from_plan_type_with_registry(
                &self.tool_registry,
                plan_type.as_str(),
            )
            .ok()
        } else {
            None
        }
    } else {
        None
    };
    // ...
}
```

## 2) The FSM discipline is enforced at runtime, not only in prompts

The file executes typed tool session plans and enforces sequencing semantics (`Open -> Send -> Next -> Finish/Abort`).

This is the difference between "guidance" and "guarantee": prompts may suggest the sequence, but runtime code enforces it.

## 3) Execution context is first-class

`baml.rs` threads scope metadata (context IDs, correlation IDs, task/message metadata) through tool execution paths. This is what makes provenance, tracing, and incident analysis practical.

A lot of systems bolt this on later. Here, it is part of the execution model.

## 4) Failure modes are explicit

When plan-to-tool resolution fails, the runtime throws hard errors with actionable guidance instead of silently guessing. That decisiveness is architecturally healthy for agent systems.

## How to Read It Effectively

1. Start at `load_schema` to see how runtime state is initialized.
2. Read `invoke_function` to understand the main call path.
3. Read `execute_tool_from_baml_result_or_value` to see model-output-to-tool bridging.
4. Read `execute_tool_session_plan` to understand the enforced session protocol.
5. Then jump to `tool_extraction.rs` for input/plan parsing constraints.

## Final Thought

`baml.rs` is the project's contract boundary between probabilistic reasoning and deterministic execution. If you appreciate that boundary, you understand the architecture.
