# Agent Runner Reference

The `baml-agent-runner` binary provides HTTP API endpoints for agent execution, A2A communication, and system management.

## Core Endpoints

### Agent Execution
- `POST /agents/{agent_id}/invoke` — Execute agent with conversation context
- `GET /agents/{agent_id}/stream` — SSE stream for agent execution events
- `POST /agents/{agent_id}/cancel` — Cancel running agent execution

### A2A (Agent-to-Agent) Communication
- `POST /a2a` — JSON-RPC A2A calls with SSE response stream (no separate `/a2a/sse` route)
- `GET /a2a/status` — A2A subsystem health check

### Repository Management
- `POST /repository/upload` — Upload agent package (tar.gz)
- `GET /repository/agents` — List available agents
- `GET /repository/agents/{agent_id}` — Agent metadata and status
- `DELETE /repository/agents/{agent_id}` — Remove agent package

### System Management
- `GET /health` — Basic health check
- `GET /metrics` — Prometheus metrics endpoint
- `POST /shutdown` — Graceful shutdown

## Configuration

### Cluster Mode
Enable with `--surreal-endpoint <URL>` (not `RUNNER_TOKEN` alone). Requires SurrealDB for shared state.

### Environment Variables
- `RUNNER_TOKEN` — Authentication token for API access
- `BAML_LOG` — Logging configuration (default: `info`)
- `OTEL_EXPORTER_OTLP_ENDPOINT` — OpenTelemetry collector endpoint

### Command Line Options
```bash
baml-agent-runner [OPTIONS]

OPTIONS:
    --port <PORT>                    HTTP server port [default: 8080]
    --host <HOST>                    Bind address [default: 0.0.0.0]
    --surreal-endpoint <URL>         SurrealDB endpoint for cluster mode
    --surreal-namespace <NS>         SurrealDB namespace [default: agentium]
    --surreal-database <DB>          SurrealDB database [default: runtime]
    --repository-path <PATH>         Local repository storage path
    --max-concurrent-agents <N>      Maximum concurrent agent executions
```

## Context Compaction

The runner implements settlement-driven context compaction to manage conversation memory:

### Compaction Triggers
- Token count thresholds (configurable per agent)
- Settlement events (tool completion, conversation milestones)
- Manual compaction requests via API

### Wire Reference Validation
Context compaction preserves citation integrity through wire-ref validation:
- Validates citation references before compaction
- Maintains provenance links across compacted content
- Ensures tool outputs remain accessible post-compaction

### Compaction Process
1. **Settlement Detection** — Identifies stable conversation segments
2. **Reference Analysis** — Maps citations and tool dependencies
3. **Summarization** — Generates compact representations preserving key context
4. **Validation** — Ensures wire-ref integrity maintained
5. **Application** — Replaces original content with compacted version

## Error Handling

### HTTP Status Codes
- `200` — Success
- `400` — Bad Request (invalid parameters)
- `401` — Unauthorized (missing/invalid token)
- `404` — Not Found (agent/resource not found)
- `409` — Conflict (agent already running)
- `500` — Internal Server Error
- `503` — Service Unavailable (overloaded)

### Error Response Format
```json
{
  "error": {
    "code": "AGENT_NOT_FOUND",
    "message": "Agent 'example-agent' not found in repository",
    "details": {
      "agent_id": "example-agent",
      "available_agents": ["other-agent"]
    }
  }
}
```

## Monitoring

### Health Checks
- `/health` — Basic liveness check
- `/health/ready` — Readiness check (includes repository access)
- `/health/deep` — Deep health check (includes SurrealDB connectivity)

### Metrics
Prometheus metrics available at `/metrics` endpoint. See [metrics inventory](metrics-inventory.md) for complete list.

### Logging
Structured JSON logging with configurable levels. Key log contexts:
- `agent_execution` — Agent lifecycle events
- `a2a_communication` — A2A call tracing
- `context_compaction` — Memory management operations
- `repository_management` — Package operations

## Security

### Authentication
Bearer token authentication via `Authorization: Bearer <token>` header or `RUNNER_TOKEN` environment variable.

### Rate Limiting
Configurable per-endpoint rate limiting to prevent abuse.

### Input Validation
Strict validation of all inputs including:
- Agent package format validation
- Conversation structure validation
- A2A message schema validation

## Development

### Local Testing
```bash
# Start runner with debug logging
BAML_LOG=debug cargo run --bin baml-agent-runner -- --port 8080

# Upload test agent
curl -X POST http://localhost:8080/repository/upload \
  -H "Authorization: Bearer test-token" \
  -F "package=@test-agent.tar.gz"

# Execute agent
curl -X POST http://localhost:8080/agents/test-agent/invoke \
  -H "Authorization: Bearer test-token" \
  -H "Content-Type: application/json" \
  -d '{"conversation": {"messages": []}}'
```

### Integration Testing
See `crates/baml-rt-api/tests/` for comprehensive integration test suite covering all endpoints and scenarios.
