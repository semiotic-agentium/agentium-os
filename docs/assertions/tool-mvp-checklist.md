# Host Tool MVP Checklist

Use this checklist before merging a new host tool.

## Product & UX
- Read-only vs write actions explicitly defined.
- User-visible output includes source links when possible.
- Missing or stale info is called out explicitly.

## API & Data
- Required inputs validated (per action).
- IDs are sanitized (UUID/allowed pattern) before use in URLs.
- Pagination supported or documented as a limitation.
- Response shape robust to partial/malformed items.

## Errors & Reliability
- API errors mapped to clear `BamlRtError` variants.
- Deserialization errors are distinct from HTTP errors.
- Rate limits surfaced clearly.

## Security & Auditability
- Secrets declared via `ToolSecretRequirement`.
- Secrets never logged.
- Output includes stable source identifiers (IDs + URLs).

## Tooling & Integration
- Tool is registered in `baml-agent-runner`.
- Tool is allowlisted in agent `manifest.json`.
- Generated artifacts updated if required.
- Session FSM follows `Open` / `Send` / `SearchRead` / `PageRead` / `Finish` / `Abort` (see [host-tool-guide](../reference/host-tool-guide.md)).

## Tests (minimum)
- Action validation unit tests.
- Error mapping test for a failed API call (mock or fixture).
- Snapshot / interface generation test if schema changes.
