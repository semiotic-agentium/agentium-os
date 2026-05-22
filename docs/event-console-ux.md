# Event Console — operator UX

The Event Console follows a **CI run page** pattern: a thin control shell, a status strip, a linear provenance transcript (shared with Chat components), and a **centered modal** for publishing.

## Control shell

- **Run** — Pick a prior `context_id` or publish a new event.
- **Status pill** — Publishing, waiting for ingress, live, failed.
- **Context** — Click the chip or Copy to copy `context_id`.
- **New event** — Opens the publish modal (agent, source payload, scope, JSON batch).

## After publish

- **Status banner** — Shows how many subscribers accepted the event and lists per-agent failures.
- **Transcript** — Host ingress (wire JSON) and agent steps appear as they are recorded.
- **Traces** — Expand the right rail once a run is selected or after publish.

## Validate vs publish

Use **Validate** in the modal to check the draft against `POST /event-dispatch/validate`. **Publish event** calls `POST /events/publish`, which fans out to all matching subscribers (not only the agent selected for validation).
