# Repository API Contract

## Scope

This document defines repository contracts for publish/entries/blob behavior.

## Canonical Artifact Identity

- Hash algorithm: `sha256`
- Hash input: canonical source bundle content (manifest + source files)
- Hash encoding: lowercase hex string

`content_hash = sha256(canonical_source_bundle)`

## Publish Ownership Rule

Publish is source-first and server-owned:

1. Client sends source bundle: `POST /repository/publish`
2. Repository assigns version and computes canonical `content_hash`
3. Host/repository publish orchestrator builds deployable artifact from source
4. Built bytes are stored under `content_hash`

Clients do not upload arbitrary blobs to establish publish provenance.

## HTTP Semantics (MVP)

- `400 Bad Request`: malformed payload, invalid hash format
- `404 Not Found`: requested entry/blob not found
- `409 Conflict`: `(agent_name, version)` conflict

## Required Repository Endpoints (MVP)

- `GET /repository/blobs/{hash}`
- `POST /repository/publish`
- `GET /repository/entries`
- `GET /repository/entries/{hash}`
- `GET /repository/entries?name=<name>&version=<version>`

## Tag Source of Truth

- Tags are defined in `manifest.json` only.
- Publish ingests `manifest.tags` into repository metadata.
- No standalone tag mutation endpoint in MVP.

## Search Ingestion Inputs

- `manifest_text` from manifest fields (`name`, `description`, `capabilities`, `tools`, `tags`)
- `source_text` from bounded extraction over source bundle files (best effort)
