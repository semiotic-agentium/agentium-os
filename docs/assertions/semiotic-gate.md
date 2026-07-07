# Semiotic Gate

Deterministic pre-action grounding for consequential tool calls. The LLM produces a
`ParseArtifact`; host code classifies tier and runs the P4 ambiguity-aware gate.

## Sign taxonomy (Peirce + verify)

| Sign | Weight | Meaning |
|------|--------|---------|
| symbol | 0.3 | Convention only — words constrain nothing about referent |
| index | 0.8 | Existential pin (path, URI, prod id) |
| icon | 0.9 | Structural exhibit (schema, predicate) |
| verify | 1.0 | Falsifiable postcondition |
| free | 1.0 | Unrestricted anchor |

Trojan veto: symbol-only anchor on a trojan-flagged node counts as strength 0.0 unless
icon/index/verify is also present.

## Templates and critical nodes

| Template | Critical nodes |
|----------|----------------|
| agentic_execution | ACTION, TARGET, SCOPE |
| delegation | CRITERIA, GOAL |
| code_generation | BEHAVIOR, INTERFACE |
| consequential_content | AUDIENCE, OBJECTIVE, FACTS |
| research | SUBJECT, SCOPE |

## Tier classification (declared)

Defaults from tool metadata `access_level`:

- `read` → tier 0
- `write` → tier 2
- `delete` → tier 3

Overrides: tag `semiotic:tier=N`, `semiotic:external`, `semiotic:reversible`. MCP
`readOnlyHint` / `destructiveHint` are hints only — never tier-lowering evidence.
`delegation_target` on A2A calls uses delegation default tier.

Tier 0–1: pass through (telemetry only at tier 1). Tier ≥2: requires live
`ParseArtifact` whose `covers` structurally match tool args. Tier 3 pass: human
authorization via A2A `InputRequired`.

## P4 ambiguity-aware gate

A node needs strength ≥0.8 at tier≥2 (0.3 at tier 1) iff it is **critical for its
template OR visibly ambiguous** (trojan flag, or >1 interpretation). Non-ambiguous
supporting nodes have floor 0. Tier 3 additionally requires `verify` on CRITERION when
that node exists.

Actions: `execute`, `execute_flagged`, `ask`, `queue_for_human`.

## Multi-agent invariants

- Artifacts scoped to `(agent_id, task_id)` — never inherited across delegation
- Read-only exploration manifests never reach tier≥2 gate
- Grounding does not delegate: sub-agents return proposals; orchestrator grounds

## Configuration

Runtime gate policy lives in the **config store** bundle `semiotic` — same mechanism as
LLM clients (`GET/PUT /config/semiotic`). Operators edit it in **Settings → Trust**;
changes hot-reload without restart.

**System default** applies to all agents. **Per-agent overrides** are keyed by
`agent_package` (discovered from `GET /agents`), matching LLM routing.

Default: `enabled=false`, `mode=dry_run`, `enforceMinTier=2`.

## Failure posture

Internal gate errors degrade to `ask`, never silent allow of tier≥2.

## Telemetry (structure-only v1)

`gate_decision`, `prevented_error`, `friction_denial`, `postcondition_result`, `prompt_lint`.
Friction denials (denied call re-executed unchanged) tune false-positive cost.

## Citation integrity (post-LLM, deterministic)

On LLM completion: resolve `#N`/`@N` against provenance graph. Flag unresolved refs.
No embedding similarity — replaces citation drift.

Reference implementation: `sc-review/plugin/scripts/semcomp/`.
