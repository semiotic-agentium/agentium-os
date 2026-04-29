# A2A compliance (baml-rt-api)

Reference: [Agent2Agent Protocol (A2A) Specification](https://google.github.io/A2A/specification/) (v0.2.1).

## Aligned with spec

- **Transport**: A2A communication over **HTTP(S)**. We expose:
  - **POST** `/agents/{agent_package}/{agent_instance_id}/a2a` — JSON-RPC request body (`Content-Type: application/json`); **response is SSE** (`Content-Type: text/event-stream`). Each SSE event’s `data:` line is one JSON-RPC 2.0 response object (same objects as the logical internal chunk stream).
- **Data format**: **JSON-RPC 2.0** for requests and streamed responses. Request uses `application/json`; streaming response uses **Server-Sent Events** per upstream A2A guidance.
- **Discovery**: We provide **GET** `/agents` returning a list of running agents (package, instance id, name, version). The spec allows registries/catalogs; we use a custom registry format rather than full Agent Cards at `/.well-known/agent.json`.
- **Methods**: We support A2A methods including `tasks.list`, `tasks.get`, `tasks.subscribe`, `message.sendStream` (dot-separated names; spec prose sometimes uses slashes like `message/stream` — same logical methods).

## Gaps / not implemented

- **Agent Card**: Spec recommends Agent Card at `/.well-known/agent.json` with full structure (name, description, url, capabilities, skills, defaultInputModes, defaultOutputModes, etc.). We expose a simpler discovery list at GET `/agents`.
- **Authentication**: Spec expects auth at the HTTP layer (e.g. Authorization header). We do not enforce auth in the API crate; that is left to the deployment layer.
