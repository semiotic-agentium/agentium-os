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
`runGeneratedStepExecutor(...)` resolves with a **discriminated envelope** (`outcome`). On `completed`, fields include `last`, `steps`, and `session_context` (FSM telemetry: tool choice, per-hop envelopes, structured tool output). On `agent_correctable`, use `recovery.code` / `recovery.mistake` — the promise still fulfilled. This payload is **not** the canonical operator-visible reply for provenance or UX.

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

**5) Source-Family Semantic Ingress**
When an agent subscribes to raw host-delivered events, keep the boundary explicit:
- The host owns polling, checkpoints, provenance, and dispatch.
- The source-family agent (e.g. `slack-agent` for Slack) owns source meaning and first downstream policy.
- Keep source-specific ingress logic in the agent for that source; conversational and dispatch entrypoints can share one package.

For Slack specifically, the right work unit is a conversation, not an arbitrary poll batch. Group raw records into conversation units, call `withTask({ unitKey, records })` with that slice, then use BAML and `support/slack` tool sessions inside the unit handler when the model needs thread replies or search — not host-side enrichment before history is written.

**Checklist**
Before shipping an agent:
1. Tool output is structured and schema‑validated.
2. Agent owns formatting and UX.
3. LLM prompt avoids formatting instructions unless required by schema.
4. Agent has a deterministic path for obvious inputs (IDs, links).
