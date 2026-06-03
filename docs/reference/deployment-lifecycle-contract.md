# Deployment Lifecycle Contract (Phase 0)

## Scope

This document defines MVP runner deployment contracts and runtime semantics.

## Separation of Concerns

- Repository stores immutable artifacts + metadata.
- Runner stores deployment state locally.
- Deployment state is not persisted in repository.

## Deployment Identity

- Runner internal deploy API is hash-based.
- Runtime HTTP deploy accepts:
  - `{ "hash": "..." }`
  - `{ "name": "...", "version": N }` resolved to hash in API layer
- Runtime contract identity is content-hash-based; there is no separate `runtime_version` field in `AgentManifest`.
- Manifest schema or runner contract changes propagate by content-hash change, enforcing compatibility at the hash boundary rather than via parallel version negotiation.

## Deployment Semantics (MVP)

- Re-deploy same hash is not an error.
- Repeat deploy returns success with `already_deployed=true`.
- MVP cardinality: one active deployment per `content_hash` per runner.
- Undeploy missing deployment returns `404 Not Found`.

## Runner Local State

- Backend: runner-local SurrealDB table
- Separate path from repository DB:
  - repository DB from `--repository-dir`
  - runner state DB from `--state-dir` (default `./.runner-state`)

Deployment state fields:

- `content_hash`
- `agent_name`
- `deployed_at`
- `status` (`active|failed`)
- `last_error`
- `last_attempt_at`
- `failure_count`

## Restore Policy

On runner startup:

1. Load saved deployment records.
2. Attempt restore for all saved records.
3. Continue restoring others if one fails.
4. Record per-deployment failure state for failures.
5. Clear failure state on successful subsequent boot.

Runner startup must not fail because one deployment fails restore.

## Broken Package Policy (MVP)

- No global repository `broken` flag in MVP.
- Validate at upload/publish/deploy boundaries:
  - hash format and hash/content match
  - size limits
  - extraction/manifest sanity before boot
- Failures are tracked per runner deployment record.

## Undeploy Drain Requirements

- Mark target deployment draining.
- Reject new requests during drain with `503`.
- Wait with timeout (`30s`), then force-abort if needed.

## OpenAPI Requirement

All new runtime endpoints and DTO fields must be reflected in OpenAPI/utoipa from first implementation.
