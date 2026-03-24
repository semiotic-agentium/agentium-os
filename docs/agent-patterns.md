**Agent Patterns**

**Onboarding:** [How to write agents](how-to-write-agents.md) — entrypoints, `ToolSessionPlan`, planning-oriented loops, and **history / citations**.

This doc captures the highest‑leverage patterns for building agents in this
codebase. It is written for coding agents and maintainers, and aims to make
behavior deterministic, testable, and easy to evolve.

**Core Principle**
Tools return structured data. Agents render user‑facing output.

This separation keeps the runtime/tool layer stable while allowing the agent
layer to own UX. It avoids stringly‑typed contracts and makes tests reliable.

**Step executor vs chat reply (provenance)**
`runGeneratedStepExecutor(...)` returns **FSM execution telemetry** (`last`, `steps`,
`session_context`, …): tool choice, per‑hop envelopes, and structured tool output.
It is **not** the canonical operator‑visible reply for provenance or UX.

The **single** user‑facing artifact the platform surfaces and records is the agent
chat handler’s return value: `SessionResult.message` (typically `StructuredReply`
with `parts` + `citations`). Do **not** duplicate that reply onto step records or
scrape `run.last` as a substitute—synthesize once at session completion (or emit
streaming chunks via `ctx.emit` when the product requires mid‑turn text).

**Why This Matters**
1. Determinism: Structured outputs can be validated and unit‑tested.
2. Observability: Tool results can be traced independently of formatting.
3. UX control: The agent can adapt formatting without changing tool behavior.
4. Change isolation: Tool schema changes don’t require prompt rewrites.

**Patterns**
**1) Structured Tool Output → Render in Agent**
- Tool returns JSON data.
- Agent formats into user‑facing strings.

Example (Notion summary):
Tool returns:
```
{
  commitments: ["Deliver demo by Friday"],
  conflicts: ["None found"],
  missing: ["Owner for security review"],
  sources: ["https://notion.so/..."]
}
```
Agent renders:
```
Commitments:
- Deliver demo by Friday
Conflicts:
- None found
Missing:
- Owner for security review
Sources:
- https://notion.so/...
```

**2) Deterministic Post‑Processing**
If you need a specific layout or normalization, do it in the agent layer:
- Sorting, deduping, truncation
- Markdown formatting
- Enforcing headings

**3) Keep Tool Prompts Focused**
Tool prompts should emphasize:
- Schema compliance
- Correct data extraction
- Avoiding prose formatting

The agent handles presentation.

**4) When to Bypass the LLM**
If the user input already contains an unambiguous identifier (e.g., Notion
page ID), the agent can call the tool directly instead of asking the LLM to
plan. This makes the path deterministic and avoids LLM indecision.

**Checklist**
Before shipping an agent:
1. Tool output is structured and schema‑validated.
2. Agent owns formatting and UX.
3. LLM prompt avoids formatting instructions unless required by schema.
4. Agent has a deterministic path for obvious inputs (IDs, links).
