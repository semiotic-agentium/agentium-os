# Drift Scoring Fixtures

Curated test scenarios for plan-anchored drift scoring. Each TOML file contains
`[[scenario]]` entries with (intent, plan step, LLM response, expected scores)
triples for deterministic unit testing and human-readable documentation.

## Categories

| File | Category | Purpose |
|------|----------|---------|
| `01_aligned_execution.toml` | Aligned | Baseline cases including pre-intent alignment |
| `02_partial_drift.toml` | Partial drift | Subtle divergence from plan — warn-level |
| `03_prompt_injection.toml` | Injection | Adversarial attacks — block-level |
| `04_plan_revision.toml` | Plan revision | Legitimate supersession vs drift |
| `05_trajectory_creep.toml` | Trajectory creep | Gradual cumulative drift over many calls |
| `06_step_boundary.toml` | Step boundary | Drift at plan step transitions |

## Schema

Each `[[scenario]]` has:
- `name` — unique identifier
- `category` — one of the category names above
- `phase` — `"pre_plan"` or `"plan_committed"` (determines which assessment DU variant applies)
- `description` — human-readable explanation of what this scenario tests
- `scenario.intent.description` — the declared intent text (or user message for pre-plan)
- `scenario.plan` — (plan_committed only) plan objective and revision status
- `scenario.step` — (plan_committed only) current step context
- `scenario.response.value` — JSON string of the LLM response
- `scenario.expected` — expected score ranges and severity
- `scenario.prompt.messages` — full prompt for tactical drift scoring

For `phase = "pre_plan"` scenarios, `scenario.plan` and `scenario.step` are absent.
The scorer produces `PlanDriftAssessment::PrePlan` — step alignment is structurally
absent (not zero, not None — the field does not exist on the variant).

## Adding scenarios

1. Add a new `[[scenario]]` to the appropriate category file.
2. Set `phase` to `"pre_plan"` or `"plan_committed"`.
3. Run `cargo test -p baml-rt-embedding drift_fixture` to verify.
4. If the scenario reveals a threshold issue, adjust `PlanDriftConfig` defaults.
