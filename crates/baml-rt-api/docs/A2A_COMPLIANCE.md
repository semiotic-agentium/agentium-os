# A2A compliance (baml-rt-api)

Reference: [Agent2Agent Protocol (A2A) Specification](https://google.github.io/A2A/specification/) (v0.2.1).

## Aligned with spec

- **Transport**: A2A communication over **HTTP(S)**. We expose:
  - **POST** `/agents/{agent_package}/{agent_instance_id}/a2a` — JSON-RPC request body, JSON-RPC response(s) in body.
  - **POST** `/agents/{agent_package}/{agent_instance_id}/a2a/sse` — JSON-RPC request body; response is **Server-Sent Events** (`Content-Type: text/event-stream`). Each event’s `data` field is one JSON-RPC 2.0 response. Chunking: one event per response. Liveness: keep-alive comments sent on an interval (15s) while the stream is open.
- **Data format**: **JSON-RPC 2.0** for requests and responses. `Content-Type: application/json` for POST body; SSE response uses `text/event-stream`.
- **Discovery**: We provide **GET** `/agents` returning a list of running agents (package, instance id, name, version). The spec allows registries/catalogs; we use a custom registry format rather than full Agent Cards at `/.well-known/agent.json`.
- **Methods**: We support A2A methods including `tasks.list`, `tasks.get`, `tasks.subscribe`, `message.sendStream` (dot-separated names; spec prose sometimes uses slashes like `message/stream` — same logical methods).

## Gaps / not implemented

- **Agent Card**: Spec recommends Agent Card at `/.well-known/agent.json` with full structure (name, description, url, capabilities, skills, defaultInputModes, defaultOutputModes, etc.). We expose a simpler discovery list at GET `/agents`.
- **Authentication**: Spec expects auth at the HTTP layer (e.g. Authorization header). We do not enforce auth in the API crate; that is left to the deployment layer.
