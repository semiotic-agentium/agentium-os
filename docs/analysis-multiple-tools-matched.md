# Analysis: "Multiple tools matched input schema"

## Origin of the error

**Error message:** `Invalid argument: Multiple tools matched input schema: system/discover_tools, system/internal_a2a, system/discover_agents`

**Source:** `crates/baml-rt-quickjs/src/baml/tool_extraction.rs`, in `resolve_tool_name_from_input_with_registry()` (lines 68–94). It is only raised when `matches.len() > 1`.

## Call path

1. **execute_tool_from_baml_result_or_value** (`baml.rs`)
   BAML returns a **session plan** (steps: Open, Next, Finish). It calls `resolve_tool_name_from_plan_steps(&plan)` to get the tool name, then `execute_tool_session_plan(scope, tool_name, plan)`.

2. **resolve_tool_name_from_plan_steps** (`baml.rs` ~1172)
   Takes the **first** Open or Send step’s payload:
   - From **Open**: `initial_input` (e.g. `{ "query": "calc", "limit": 10 }` for discover_tools).
   - From **Send**: `input`.
   Then calls `resolve_tool_name_from_input(input)` with that value.

3. **resolve_tool_name_from_input** (`baml.rs` ~1192)
   Forwards to `resolve_tool_name_from_input_with_registry(registry, input)`.

4. **resolve_tool_name_from_input_with_registry** (`tool_extraction.rs` ~68)
   - Calls `registry.all_metadata()` (all registered tools).
   - For each tool, checks `input_matches_schema(input, &metadata.input_schema)`.
   - If exactly one tool matches → returns that tool name.
   - If more than one match → returns the error above.

So the **input** being matched is the **Open step’s `initial_input`** (e.g. `{ "query": "calc", "limit": 10 }`).

## Root cause

**Wrong schema is used for matching.**

- The value passed in is the **Open** step’s `initial_input`.
- Matching is done only against **`metadata.input_schema`**, which is the **Send-step** input schema for each tool.
- It is **not** matched against **`metadata.open_input_schema`** (the Open-step schema).

So we are matching “Open payload” against “Send payload” schemas. For the three system tools:

- **discover_tools**: Open = `DiscoverToolsOpenInput` (query?, limit?); Send = `DiscoverToolsSendInput` (query?, text?).
- **discover_agents**: Open = `DiscoverAgentsOpenInput` (query?, limit?, offset?); Send = `DiscoverAgentsSendInput` (…).
- **internal_a2a**: Open = `InternalA2aOpenInput` (target: { agent_package, agent_instance_id }); Send = `InternalA2aSendInput` (parts?, text?).

Generated JSON schemas for these Send types typically have `"type": "object"` and **no** (or very few) **required** properties.

**input_matches_schema** (same file, ~96) only:

1. Requires `input` and `schema` to be objects.
2. If `schema.type` is present and not `"object"`, returns false.
3. If `schema.required` is present, checks that every required key exists in `input`.
4. Otherwise returns true.

So for a schema with `type: "object"` and no `required` (or empty `required`), **any** object passes. The Open payload `{ "query": "calc", "limit": 10 }` therefore matches **all three** tools’ **Send** schemas → three matches → “Multiple tools matched input schema”.

## Summary

| What we do today | What we should do |
|------------------|-------------------|
| Take first Open/Send payload from the plan | Same |
| Match that payload against every tool’s **input_schema** (Send) | When the payload came from an **Open** step, match against **open_input_schema**. When it came from a **Send** step, match against **input_schema**. |

So the bug is **schema phase mismatch**: Open-step input is matched against Send-step schemas, which are too permissive and not specific to each tool’s Open shape, causing multiple tools to match.

## Fix (high level)

- In **resolve_tool_name_from_plan_steps**, record whether the first input came from an **Open** or a **Send** step.
- When resolving by input, pass that phase (open vs send) into the resolver.
- In **resolve_tool_name_from_input_with_registry** (or a variant), use **open_input_schema** when phase is Open and **input_schema** when phase is Send.
- Then the Open payload `{ "query": "calc", "limit": 10 }` is matched only against each tool’s **open_input_schema**; only discover_tools’ Open schema should match that shape, giving a single match.
