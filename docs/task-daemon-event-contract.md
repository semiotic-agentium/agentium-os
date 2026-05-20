# Task-daemon event contract (source-records)

Task-daemon polls external sources and publishes **`host.source-records.v1`** to the runner via `POST /events/publish`. The runner records provenance (`HostSourcePollRecorded`, `HostDispatchAccepted`) and fans out to subscribed agents on routing key **`event:intake`**.

## Wire envelope

- `schema_version`: `host.source-records.v1`
- `routing_key`: `event:intake`
- `source_kind` / `source_key`: subscription matching (e.g. `slack`, `slack:C123`)
- `context_id`: stable per poll window (minted from `source_kind`, `source_key`, `source_cursor`)
- `message_id`: stable poll batch id (`td-poll-batch-*`)
- `messages[]`: one JSON batch per poll (Slack history rows, ClickUp lifecycle records, etc.)

## Provenance

Lineage is stored in Surreal provenance, not in a parallel interpretation JSON contract. Agents perform semantic work in `onDispatch` against the source-records batch.

## Removed

`task-daemon.interpretation.v1`, `InterpretationRequestEvent`, `InterpretationResultEvent`, and in-daemon LLM extraction are removed.
