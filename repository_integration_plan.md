# Repository Integration Implementation Plan

## Purpose

Implement repository-driven deployment while **preserving `.tar.gz` as the canonical distributable artifact**.

Today:
- Agent source lives in `agents/<agent-name>/...`
- Builder creates `.tar.gz`
- Runner loads packages from CLI file paths

Target:
- Builder still creates `.tar.gz`
- Repository stores metadata + blob bytes + search
- Runner deploys by repository identity (hash or name/version), not local file path
- Runner keeps local deployment state (separate concern from repository)

---

## Core Concepts (must remain true)

1. Source is not artifact:
- Source is editable files under `agents/`.
- Artifact is immutable built `.tar.gz`.

2. Artifact is not deployment:
- Artifact can exist in repository without being deployed.
- Deployment is runner-local runtime state.

3. Repository is archive/provenance:
- Owns metadata, search, and blob storage.
- Does not own runner deployment lifecycle state.

4. `.tar.gz` remains first-class:
- Still produced for portability/distribution.
- Can be pushed/pulled independent of runtime deployment.

---

## Phase Strategy

Each phase should be:
- Small and isolated
- Mergeable with passing tests
- Behaviorally complete for its own scope

Rule: **One phase per PR** unless phase is tiny and clearly non-risky.

---

## Implementation Checklist (Cross-Session Tracking)

### Phase 0: Contracts and Config
- [x] Document canonical hash rule: `sha256(tar_gz_bytes)` (lowercase hex).
- [x] Document blob size default (`5 MB`) and config/env override (`repository.max_blob_bytes`, `BAML_REPOSITORY_MAX_BLOB_BYTES`).
- [x] Document deploy semantics (`already_deployed=true`, undeploy missing -> `404`, one deployment per hash per runner).
- [x] Document separate DB paths (`--repository-dir`, `--state-dir`).
- [x] Document OpenAPI/utoipa update requirement for every new endpoint/DTO.

### Phase 1: SurrealDB Repository Backend
- [x] Replace repository SQLite/FS backend with SurrealDB-only backend implementation.
- [x] Remove old repository backend modules and wiring.
- [x] Port repository tests to in-memory SurrealDB.
- [x] Remove fitness domain/model/API/store concerns from MVP scope.

### Phase 2: Repository Publish/Entries/Blobs + Search Inputs
- [x] Implement `PUT /repository/blobs/{hash}` and `GET /repository/blobs/{hash}`.
- [x] Implement `POST /repository/publish` (blob-first policy enforced).
- [x] Implement `GET /repository/entries`.
- [x] Implement `GET /repository/entries/{hash}`.
- [x] Implement `GET /repository/entries?name=<name>&version=<version>`.
- [x] Enforce payload/hash validation + HTTP status mapping (`400`, `404`, `409`).
- [x] Enforce max blob size checks (`5 MB` default).
- [x] Populate `manifest_text` from manifest metadata at publish time.
- [x] Populate `source_text` via bounded text extraction from tarball at publish time.
- [x] Ensure publish fails when referenced blob hash is missing.
- [x] Implement `cargo agent-platform publish` with blob-first workflow (`package.tar.gz path -> PUT blob -> POST publish`).
- [x] Ensure `POST /repository/publish {blob_hash}` stores and returns the same hash identity.

### Phase 3: Runner Local State
- [x] Add runner-local SurrealDB deployment table.
- [x] Add deployment record fields (`content_hash`, `agent_name`, `deployed_at`).
- [x] Add failure tracking fields (`status`, `last_error`, `last_attempt_at`, `failure_count`).
- [x] Wire `--state-dir` configuration and defaults.

### Phase 4: Runtime Deploy Core
- [ ] Remove CLI package-path startup flow.
- [x] Add `DeploymentManager` trait in `baml-rt-core`.
- [x] Implement hash-based deploy/undeploy core in runner.
- [x] Implement startup restore loop (try all saved deployments).
- [x] Record per-deployment restore/deploy failures in local state.
- [x] Clear failure state on successful subsequent boot.
- [ ] Implement graceful drain on undeploy (`503` for new requests, timeout, force-abort).
- [x] Apply hot-deploy lock discipline (boot outside write lock, insert/remove under short write lock).

### Phase 5: Runtime HTTP API
- [x] Add `POST /deploy`.
- [x] Add `POST /undeploy`.
- [x] Add `GET /deployments`.
- [x] Resolve `{name, version}` to hash in API layer before runner deploy call.
- [x] Enforce one-active-deployment-per-hash semantics (`already_deployed=true` on repeat deploy).
- [x] Include deployment status/error fields in deployments response.
- [x] Mount repository routes under `/repository`.
- [x] Update OpenAPI/utoipa for all new runtime and repository endpoints/DTOs.

### Phase 6: Metadata Propagation
- [x] Enrich runner in-memory deployed-agent model with repository provenance.
- [x] Add provenance fields to `AgentCard` and API/system DTOs (`content_hash`, `repository_version`, `tags`).
- [ ] Verify serialization and discovery output coverage.

### Phase 7: Builder CLI
- [ ] Implement/complete `push` (build -> upload blob -> publish metadata -> optional deploy).
- [ ] Implement/complete `pull` (resolve -> download blob -> optional extract).
- [ ] Implement/complete `deploy`.
- [ ] Implement/complete `undeploy`.
- [ ] Enforce blob-first ordering in CLI workflow.

### Phase 8: Meta Tools + UI (optional)
- [ ] Implement `meta/search_repository`.
- [ ] Implement `meta/deploy_agent`.
- [ ] Add repository/deployment UI views and wiring.

### Phase 9: Fuzzing
- [ ] Add fuzz target for blob ingest/extract path.
- [ ] Add fuzz target for repository API payload parsing/validation.
- [ ] Add fuzz target for hash/content consistency checks.
- [ ] Add fuzz target for runner startup restore state handling.
- [ ] Add scheduled CI fuzz run and failure visibility.

### Cross-Cutting Manifest Tagging Work
- [x] `[MANIFEST-TAGS]` Add `tags: []` field to agent manifest schema and supported manifests.
- [x] `[MANIFEST-TAGS]` Update `cargo-agent-platform new-agent` generator to scaffold `tags: []` in new `manifest.json`.
- [x] `[MANIFEST-TAGS]` Add interactive tool-based tag suggestions in `cargo-agent-platform new-agent` (exclude generic tags).
- [x] `[MANIFEST-TAGS]` Enforce non-empty tags and reject banned generic tags (`support`, `read`, `write`, `system`) in `new-agent`.
- [x] `[MANIFEST-TAGS]` Enforce manifest tags contract in `cargo-agent-platform doctor` for `agents/` and fixture agents.
- [x] `[MANIFEST-TAGS]` Ensure publish ingests manifest tags and FTS includes tags in `manifest_text`.

---

## MVP Decisions Locked (from discussion)

1. Hashing
- Algorithm: `SHA-256` (industry-standard default, widely supported).
- Canonical input: raw bytes of the final packaged `.tar.gz` artifact.
- Encoding: lowercase hex string.

2. Artifact size limits
- Initial maximum upload size: `5 MB` per package blob.
- This is a configurable operational limit and can be adjusted later.

3. Deploy/Undeploy HTTP semantics
- Re-deploying an already deployed package is **not an error**.
- `POST /deploy` returns success with explicit `already_deployed=true` when applicable.
- `POST /undeploy` for missing target returns `404 Not Found`.
- MVP deployment cardinality is one active deployment per `content_hash` per runner.

4. Version uniqueness and idempotency
- Enforce unique `(agent_name, version)` in repository metadata.
- Blob idempotency by hash: if hash already exists, do not rewrite.
- On version conflict, reject publish with `409 Conflict` (do not deploy/store conflicting metadata).

5. Version retention
- Keep all historical versions by default (no automatic pruning in MVP).
- Garbage collection/retention policy is deferred.

6. Runner deployment state backend
- Use runner-local SurrealDB table (not JSON file).
- Use a separate path from repository DB:
  - repository DB from `--repository-dir`
  - runner state DB from `--state-dir` (default `./.runner-state`)

7. Restore policy
- On startup restore failure for a single saved deployment, continue restoring remaining deployments.
- Mark failed restores as degraded and surface them via logs/status.
- Do not fail the entire runner startup due to one restore failure.

8. Broken-package handling policy
- Do not add a global repository-level `broken` flag in MVP.
- Perform early validation at upload/publish and deploy boundaries:
  - hash format and hash-content match
  - blob size limit
  - extraction/manifest sanity check prior to boot
- Track failures per runner deployment record (`status`, `last_error`, `last_attempt_at`, `failure_count`).
- Always retry saved deployments on subsequent restarts.
- On successful boot, clear failure state and mark deployment `active`.

9. Lineage metadata
- Removed from MVP metadata and API scope.
- Future lineage support (if reintroduced) should be captured structurally via official creation/publish flows, not user-entered at deploy time.

10. Endpoint protection (temporary mitigation)
- Keep auth design open for now.
- Add operational mitigation: prefer localhost-only exposure for deploy/admin endpoints where feasible.
- TODO: introduce proper authentication/authorization in a follow-up phase.

11. Tag source of truth
- Tags are defined in `manifest.json` only (artifact source of truth).
- No user-entered tags in deploy APIs.
- No standalone tag mutation endpoint in MVP.
- Publish ingests tags from manifest into repository metadata.

13. Publish metadata source and origin policy (MVP simplification)
- Publish metadata should be derived from the uploaded blob/manifest whenever possible.
- Do not require `origin` semantics (`original`/`iteration`/`influenced`) in MVP publish workflow.
- Treat publish as local repository registration/indexing by default (not global distribution).
- Keep publish request minimal and avoid duplicate user-supplied manifest metadata.

12. Blob size configurability
- Default max blob size is `5 MB`.
- Config key: `repository.max_blob_bytes`.
- Env override: `BAML_REPOSITORY_MAX_BLOB_BYTES`.

---

## Phase 0: Contract Freeze (No behavior change)

### Goal
Lock data contracts and boundaries before refactors.

### Scope
- Finalize shared types/fields:
  - `content_hash`
  - `repository_version`
  - `tags`
- Define deployment API request/response payloads.
- Define repository publish/blob/entry API payload semantics.
- Document canonical hash rule: `sha256(tar_gz_bytes)`.
- Document initial blob size limit: `5 MB`.
- Document config and env override for blob limit.
- Document tag ingestion from manifest as the only MVP tagging source.

### Files likely touched
- `repository_integration.md` (or docs folder for API notes)
- Optional new docs:
  - `docs/repository-api-contract.md`
  - `docs/deployment-lifecycle-contract.md`

### Exit criteria
- Team-agreed API/type contracts documented.
- No production code behavior changed.

---

## Phase 1: Repository Backend Replacement (SurrealDB only)

### Goal
Replace repository storage backend with SurrealDB, keeping trait boundaries.

### Scope
- Add `surrealdb` dependency to repository crate.
- Introduce `surreal_store.rs` implementing:
  - `MetadataStore`
  - `SearchStore`
  - `BlobStore`
- Remove SQLite/filesystem repository implementations.
- Keep `RepositoryService` interface stable.

### Files likely touched
- `crates/baml-rt-repository/Cargo.toml`
- `crates/baml-rt-repository/src/lib.rs`
- `crates/baml-rt-repository/src/surreal_store.rs` (new)
- delete `crates/baml-rt-repository/src/sqlite_store.rs`
- delete `crates/baml-rt-repository/src/fs_blob_store.rs`
- repository tests under `crates/baml-rt-repository/tests/*` or module tests

### Test gates
- Repository unit/integration tests pass with in-memory SurrealDB.
- Blob read/write roundtrip tests pass.
- Search behavior parity verified with existing tests.

### Exit criteria
- Repository crate compiles and tests pass.
- No runner wiring yet.

---

## Phase 2: Repository Blob API Hardening (`.tar.gz` lifecycle)

### Goal
Guarantee publish/read/blob workflows, including search indexing inputs, independent of runtime.

### Scope
- Ensure metadata and blob endpoints exist and are correct:
  - `POST /repository/publish` (metadata publish referencing existing blob hash)
  - `GET /repository/entries`
  - `GET /repository/entries/{hash}`
  - `GET /repository/entries?name=<name>&version=<version>`
  - `PUT /repository/blobs/{hash}`
  - `GET /repository/blobs/{hash}`
- Validate hash + payload consistency rules.
- Enforce max blob size (`5 MB`) with explicit error response.
- Enforce tags ingestion from `manifest.json` at publish time.
- Populate FTS fields at publish time:
  - `manifest_text` from manifest name/description/capabilities/tools/tags
  - `source_text` from bounded extraction of selected text files in the tarball (best effort)
- Pin HTTP semantics:
  - missing resources -> `404`
  - malformed payload/invalid hash -> `400`
  - version conflict (`agent_name`, `version`) -> `409`
- Publish atomicity policy:
  - blob-first workflow is required
  - metadata publish must fail if referenced blob hash does not exist

### Files likely touched
- `crates/baml-rt-repository/src/router.rs`
- `crates/baml-rt-repository/src/service.rs`
- DTO/error mapping files around repository HTTP layer

### Test gates
- API tests for upload/download roundtrip byte equality.
- API tests for bad hash handling and not-found behavior.
- API tests for entries list/get/query-by-name-version.
- API tests for version conflict -> `409`.
- Tests for FTS text population from manifest + extracted source.
- Tests that publish fails when blob is missing (atomicity rule).

### Exit criteria
- External clients can blob-first publish metadata + query entries + fetch blobs reliably.

---

## Phase 3: Runner Local Deployment State (isolated module)

### Goal
Introduce runner-owned persistent deployment records, separated from repository.

### Scope
- Add deployment record model:
  - `content_hash`
  - `agent_name`
  - `deployed_at`
- Add deployment health fields:
  - `status` (`active|failed_restore|failed_deploy`)
  - `last_error`
  - `last_attempt_at`
  - `failure_count`
- Implement storage adapter (`save`, `remove`, `list`) in runner-local SurrealDB.
- Keep this module independent from deploy logic initially.

### Files likely touched
- `crates/baml-agent-runner/src/...` (new deployment state module)
- Potential config wiring in runner startup code
- runner path/config wiring for `--state-dir`

### Test gates
- Persistence tests for add/remove/list.
- Restart simulation test (write, reload, read).

### Exit criteria
- Runner can persist deployment records locally.
- No deploy/undeploy APIs yet.

---

## Phase 4: Runner Deploy/Undeploy by Repository Identity

### Goal
Switch runtime deploy path from CLI tarball paths to repository references.

### Scope
- Remove `packages: Vec<PathBuf>` from runner config/CLI.
- Remove builder loading-state path that booted from local files.
- Add `DeploymentManager` trait in `baml-rt-core` and runner implementation.
- Add:
  - `deploy_from_repository(&ContentHash)`
  - `undeploy(&AgentRouteKey)`
- Startup restore:
  - list local deployment records
  - pull blobs from repository
  - boot agents
  - always attempt all saved deployments, recording per-agent failures
- Implement graceful undeploy drain as required behavior:
  - per-agent drain signal
  - reject new requests with `503` while draining
  - timeout (30s) then force-abort if needed
- Concurrency safety requirement:
  - boot outside route-map write lock
  - take write lock only for final insertion/removal

### Files likely touched
- `crates/baml-agent-runner/src/main.rs`
- `crates/baml-agent-runner/src/builder.rs`
- runner boot/deploy modules
- `crates/baml-rt-core/src/...` for `DeploymentManager` trait

### Test gates
- Runner starts with empty deployments cleanly.
- Runner restores previously deployed agents after restart.
- Undeploy removes from routing and deployment records.
- Failed restore updates status/error/attempt/failure_count.
- Successful subsequent restore clears failure state and returns to `active`.
- Drain behavior test: new requests during undeploy return `503`.
- Drain timeout test: force-abort on timeout path.
- Concurrency test: no lock contention regression / race on hot deploy insertion.

### Exit criteria
- Runtime no longer requires CLI package paths.
- Deploy source of truth is repository identity + local deployment set.

---

## Phase 5: Runner Deployment HTTP API

### Goal
Expose runtime lifecycle operations over HTTP.

### Scope
- Add runner endpoints:
  - `POST /deploy`
  - `POST /undeploy`
  - `GET /deployments`
- Mount repository routes under `/repository`.
- Ensure deployment endpoints are distinct from repository namespace.
- Enforce agreed semantics:
  - deploy same package returns success with `already_deployed=true`
  - undeploy missing package returns `404`
- Resolve `{name, version}` in API layer via repository lookup before calling hash-based runner deploy.
- Add TODO note for endpoint protection (localhost mitigation now, auth later).
- Include deployment status/error fields in deployments response for failed restores/deploys.
- Update OpenAPI/utoipa from the start for all new repository/deploy endpoints and DTO fields.

### Files likely touched
- `crates/baml-rt-api/src/router.rs`
- API handlers/modules in runtime API crate
- request/response DTO files

### Test gates
- HTTP tests for deploy/undeploy/deployments.
- Behavior test for deploy by hash and name/version resolution.
- Test that second deploy of same hash returns existing deployment (`already_deployed=true`) and does not create another instance.

### Exit criteria
- Full deployment lifecycle accessible via HTTP.

---

## Phase 6: Metadata Propagation End-to-End

### Goal
Expose repository provenance on running agents everywhere.

### Scope
- Enrich runner in-memory agent model (`BootedAgent`).
- Enrich `AgentCard` and API/system DTOs:
  - `content_hash`
  - `repository_version`
  - `tags`

### Files likely touched
- `crates/baml-rt-core/src/agent_routing.rs`
- `crates/baml-rt-api/src/openapi.rs`
- `crates/tools/system/src/tools.rs`
- runner structs and mapping code

### Test gates
- Serialization tests for DTO compatibility.
- Discovery/list APIs include provenance for deployed agents.

### Exit criteria
- Every deployed agent can be traced to repository identity in API/tool outputs.

---

## Phase 7: Builder CLI Workflow (distribution + runtime ops)

### Goal
Make builder CLI the primary UX for artifact publication and deployment control.

### Scope
- Add or complete commands:
  - `push` (build + upload blob + publish metadata; optional deploy)
  - `pull` (resolve and download blob; optional extract)
  - `deploy`
  - `undeploy`
- Keep `.tar.gz` creation and download explicit and inspectable.
- Enforce blob-first publish flow to avoid dangling metadata entries.

### Files likely touched
- `crates/baml-agent-builder/...` CLI command modules
- HTTP client integration code for repository/runner APIs

### Test gates
- CLI integration tests against local test server.
- `push -> deploy -> undeploy -> pull` happy path.

### Exit criteria
- End users can distribute artifacts and operate deployment without local runner file paths.

---

## Phase 8: Meta Tools + Web UI (optional after backend stability)

### Goal
Expose repository/deploy features in tools and dashboard without changing backend contracts.

### Scope
- Meta tools:
  - `meta/search_repository`
  - `meta/deploy_agent`
- Web UI repository/deployment views and composables.

### Files likely touched
- `crates/tools/meta/...` (new crate/modules)
- `web/...` Vue components/composables/router/navbar

### Test gates
- Tool registration + invocation tests.
- UI integration smoke checks for repository/deployment flows.

### Exit criteria
- Operators can browse/search/deploy/undeploy via tools and UI.

---

## Phase 9: Fuzzing and Robustness Hardening (final stage)

### Goal
Validate parser/ingest/deploy surfaces against malformed or adversarial inputs before declaring MVP complete.

### Scope
- Add fuzz targets for:
  - `.tar.gz` blob ingest and extraction path used by deploy
  - repository API payload parsing/validation
  - hash/content consistency checks
  - runner startup restore from local deployment state
- Enforce safety properties:
  - no panics
  - bounded resource behavior for malformed inputs
  - deterministic rejection for invalid hash/content combinations
  - safe handling of corrupted local deployment records

### Files likely touched
- Fuzz target files under existing fuzz infrastructure (or new fuzz crate if absent)
- Ingest/extraction validation code in repository/runner modules
- CI workflow for scheduled fuzz execution

### Test gates
- Fuzz targets run successfully for fixed time budget with no crashes.
- Known malformed corpus cases are rejected with stable error handling.
- Nightly/scheduled CI job runs fuzzing and reports regressions.

### Exit criteria
- Critical ingest/deploy paths have fuzz coverage and stable behavior under malformed input.

---

## Additional Improvements I Recommend Adding

These are not mandatory for first merge, but strongly reduce risk:

1. Artifact integrity checks:
- Verify uploaded blob hash matches URL hash before storing.
- On deploy, verify blob hash again before boot.

2. Observability:
- Structured logs for publish/deploy/undeploy with hash + route key.
- Minimal metrics counters (deploy success/fail, restore success/fail).

---

## Suggested PR Sequence

1. PR-1: Phase 0 docs/contracts
2. PR-2: Phase 1 backend swap
3. PR-3: Phase 2 blob API hardening
4. PR-4: Phase 3 runner local deployment state
5. PR-5: Phase 4 deploy/undeploy runtime core
6. PR-6: Phase 5 deploy HTTP endpoints
7. PR-7: Phase 6 metadata propagation
8. PR-8: Phase 7 builder CLI commands
9. PR-9: Phase 8 meta tools/UI
10. PR-10: Phase 9 fuzzing hardening

---

## Open Questions (must be resolved during implementation)

1. Endpoint exposure defaults
- If localhost-only binding is not technically feasible in some deployments, what is the minimum acceptable temporary guard before full auth lands?

---

## Definition of Done (Program-level)

The effort is complete when all are true:

1. `.tar.gz` remains supported for build, upload, download, and distribution.
2. Runner no longer depends on CLI package file paths at startup.
3. Deploy/undeploy works by repository identity via HTTP and CLI.
4. Restart restores deployments from runner-local deployment state.
5. Deployed agents expose full repository provenance in API/tool surfaces.
6. Repository backend uses SurrealDB only (no SQLite/FS repository backend).

---

## Deferred/Removed Scope Notes (MVP Decision Record)

### Removed from MVP: `generation`

Reasoning:
- Low practical value right now because versioning already exists.
- Adds conceptual overlap and confusion (`version` vs `generation`).
- Risk of inconsistent semantics if computed/assigned differently across workflows.

Decision:
- Remove `generation` from MVP contracts, DTOs, and plan scope.

### Removed from MVP: `fitness`

Reasoning:
- Requires a real evaluation framework (tasks/datasets/metrics domains) that is not yet in place.
- Without standardized evaluation pipelines, stored scores become subjective and low trust.
- Adds implementation and governance complexity with weak immediate payoff.

Decision:
- Remove fitness tracking from MVP scope and from implementation requirements.

### Removed from MVP: `lineage` metadata

Reasoning:
- Current developer flows (including local copy/modify from git) make reliable lineage capture brittle.
- Manual lineage assignment at deploy time is low-trust and operationally weak.
- Adds schema/API and process complexity without clear short-term operational payoff.

Decision:
- Remove lineage metadata from MVP contracts and delivery scope.

Future direction (optional, non-MVP):
- Revisit lineage when `cargo-agent-platform` creation/publish flows can capture derivation structurally (for example `cargo agent-platform new-agent --from <agent>`), with machine-generated parent references.
