# Repository API Contract (Phase 0)

## Scope

This document defines MVP contracts for repository publish/entries/blob behavior.

## Canonical Artifact Identity

- Hash algorithm: `sha256`
- Hash input: raw bytes of final packaged `.tar.gz`
- Hash encoding: lowercase hex string

`content_hash = sha256(tar_gz_bytes)`

## Blob Limits

- Default max blob size: `5 MB`
- Config key: `repository.max_blob_bytes`
- Env override: `BAML_REPOSITORY_MAX_BLOB_BYTES`

## Blob-First Publish Rule

MVP publish flow is blob-first:

1. Upload blob: `PUT /repository/blobs/{hash}`
2. Publish metadata referencing that hash: `POST /repository/publish`

If referenced blob hash does not exist, publish must fail.

## HTTP Semantics (MVP)

- `400 Bad Request`: malformed payload, invalid hash format, hash/content mismatch
- `404 Not Found`: requested entry/blob not found
- `409 Conflict`: `(agent_name, version)` conflict

## Required Repository Endpoints (MVP)

- `PUT /repository/blobs/{hash}`
- `GET /repository/blobs/{hash}`
- `POST /repository/publish`
- `GET /repository/entries`
- `GET /repository/entries/{hash}`
- `GET /repository/entries?name=<name>&version=<version>`

## Tag Source of Truth

- Tags are defined in `manifest.json` only.
- Publish ingests `manifest.tags` into repository metadata.
- No standalone tag mutation endpoint in MVP.

## Search Ingestion Inputs (MVP)

- `manifest_text` from manifest fields (`name`, `description`, `capabilities`, `tools`, `tags`)
- `source_text` from bounded text extraction from tarball (best effort)

