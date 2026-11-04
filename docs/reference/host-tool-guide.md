# Host Tool Development Guide

Host tools in Karakara OS provide agents with capabilities beyond LLM inference. This guide covers both **MCP (Model Context Protocol) servers** and **external tools** — the two primary host tool types.

## Tool Types Overview

### MCP Servers
- **Protocol**: JSON-RPC over stdio/SSE
- **Lifecycle**: Managed by Karakara runtime
- **State**: Stateless request/response
- **Use cases**: APIs, databases, file systems
- **Registry**: Cached server snapshots with schema validation

### External Tools (v2 toolsets)
- **Protocol**: MCP — stdio to a supervised worker (Sandbox/Trusted) or Streamable HTTP (Service)
- **Unit**: one `.ktool` = one MCP server = N tools (a toolset)
- **Lifecycle**: a long-lived worker per toolset; microVM (Sandbox), host process (Trusted), or external endpoint (Service)
- **Identity**: two digests — `descriptor_digest` (interface) and `tool_digest` (exact bytes); approvals are records keyed by `tool_digest`
- **Registry**: content-addressed toolset snapshots with operator approval

## MCP Server Integration

### Server Configuration

MCP servers are configured in `mcp-catalog.json`:

```json
{
  "servers": {
    "filesystem": {
      "command": "npx",
      "args": ["@modelcontextprotocol/server-filesystem", "/tmp"],
      "env": {}
    },
    "postgres": {
      "command": "mcp-server-postgres",
      "args": ["postgresql://user:pass@localhost/db"]
    }
  }
}
```

### Registry Snapshots

Karakara maintains registry snapshots of MCP servers for:
- **Schema validation** - Ensure tool/resource schemas match expectations
- **Offline builds** - Enable CI/test without external dependencies  
- **Version consistency** - Lock to approved server versions

Snapshot resolution:
1. Check registry for cached server snapshot
2. Fall back to live server discovery if not cached
3. Validate schema compatibility

### BAML Integration

MCP tools are exposed as BAML functions:

```typescript
// Generated from MCP server schema
function ReadFile(path: string) -> string
function WriteFile(path: string, content: string) -> string
function ListDirectory(path: string) -> string[]
```

Usage in agents:

```typescript
import { ReadFile, WriteFile } from "./baml_client"

const content = await ReadFile("/path/to/file.txt")
const result = await WriteFile("/output.txt", processedContent)
```

## Generated tool DTO schemas

The builder generates Open, Send, and output DTOs in both BAML and TypeScript from the three JSON Schema values in resolved `ToolFunctionMetadata`, lowered once into `ToolSchemaGraph`. Static, external, MCP, sandbox, and WASM tools follow the same path; origin and backend never select a different renderer. A tool's `baml_decl`, `ToolTypeSpec.ts_decl`, and `extra_ts_decls` still travel in catalog records for compatibility, but neither the BAML nor the TypeScript generator reads them.

Generated TypeScript also owns `ToolName`, the input/output/open-input maps, and `openToolSession`. Open input is required at the call site when the tool schema requires fields, optional when the schema allows an empty object, and omitted when the tool uses the unit Open input. Opaque schemas render as `JsonValue`. Do not add a local `declare function openToolSession` shim for a manifest-backed tool.

The schema graph supports primitives, literals, homogeneous enums, arrays, typed maps, fixed objects, local `$defs` or `definitions` references, nullable values, `oneOf`, `anyOf`, `vec_or_one`, tagged object unions, and `OpaqueJson`. The builder rejects external `$ref` values, boolean schemas, unresolved local references, empty or unsupported objects, and other shapes it cannot represent in BAML. Use `OpaqueJson` when a tool accepts arbitrary JSON rather than publishing an unrepresentable schema.

## External Tools (v2 — toolsets)

External tools run outside the OS process as **MCP servers**. The unit of
distribution, approval, capability grant, and identity is the **toolset**: one
`.ktool` bundle = one MCP server = N tools.

### Tiers

| Tier | Where the worker runs | Transport to the worker |
|---|---|---|
| `Static` | compiled into the OS | in-process (first-party only) |
| `Sandbox` | microVM (microsandbox) | MCP stdio, via the guest supervisor |
| `Trusted` | host process, operator-vouched | MCP stdio, via a host-side supervisor (`baml-tool-supervisor --tier trusted --uds`) |
| `Service` | external endpoint the operator runs | MCP Streamable HTTP |

Every non-Static tool is an MCP server; the tiers differ only in *where* the
supervisor runs. The three protocol layers are: runner ↔ supervisor
(`protocol_v2`), supervisor ↔ worker (**MCP stdio**), and runner ↔ Service
(**MCP HTTP**).

### Authoring (TypeScript)

`@karakara/tool` is thin sugar over the official `@modelcontextprotocol/sdk`; a
plain MCP server with no Karakara import is equally valid. Toolset-level
capabilities live in `karakara.toml` next to `package.json` (they are a property
of the one process the `.ktool` ships, not of an individual tool):

```toml
[toolset]
name = "support"
version = "1.2.0"

[[toolset.capabilities.net]]
host = "api.example.com"
port = 443

[[toolset.capabilities.secrets]]
name = "EXAMPLE_API_KEY"
```

Workflow:

```bash
karak tool new --lang ts ./tools/support   # scaffold an MCP-server toolset
karak tool build ./tools/support           # runs the server, reads tools/list, assembles the .ktool
karak tool publish ./tools/support          # host audits the bundle, records the approval
```

`karak tool build` asks the built MCP server for `tools/list` and records each
tool's canonical descriptor into `tools.json`; MCP annotations map to Karakara
semantics (`readOnlyHint` ⇒ Read, `destructiveHint` ⇒ Irreversible, else Write;
`idempotentHint`). Other languages follow with their runtime images.

### Trusted placement

A `Trusted` toolset runs unsandboxed as a crash-isolated host process under one
operator-trust record; there are no per-device or per-path grants. Declare where
its supervisor lives in the same file, then publish with `--trusted`:

```toml
[toolset]
name = "gpu"
version = "0.1.0"

[toolset.trusted]
mode = "attach"                       # or "spawn"
socket_path = "/run/karakara/gpu.sock" # attach only; must be absolute
node = "gpu-1"                         # omit for the local node
```

`attach` connects to a supervisor the operator already runs (hardware daemons);
`spawn` lets the runner start its own supervisor for the workflow.
`karak tool publish --trusted` records the declaration in `ktool.json`, so it is
part of `tool_digest`. Deploy then checks the declared node and socket, and
fails with `tool '<name>' (<digest>) requires reachable node|endpoint '<target>',
which is not reachable` instead of building or moving the agent.

### Approval and audit records

A Trusted bundle needs two approvals. The snapshot must be approved, and the
operator policy must trust the exact `(agent_id, tool_digest)` pair. Requested
network or secret capabilities remain informational on this path. Trusted
execution gives the process host access, so the policy does not turn those
requests into a partial grant.

The runner sends these lifecycle records to provenance:

- `TrustGranted` when an approved Trusted bundle passes the operator-trust
  check. The record includes the exact `tool_digest` and node.
- `TrustedInvoke` for each invocation that runs under that trust record. It
  carries the agent artifact, context, task, invocation, and trace IDs supplied
  by the host.
- `TrustRevoked` when the operator removes the exact agent and bundle pairing.
  The record includes the revoked `tool_digest`.
- `ServiceBound` after a Service endpoint passes its deploy-time schema check.

A failed node, socket, or Service endpoint check stops deployment. The runner
does not build a missing tool or silently choose another endpoint.

### Identity: two digests

Each tool carries two independent digests:

- **`descriptor_digest`** — the tool's *interface* (name, schemas, side effect,
  idempotency, output trust). It is the only tool input to an agent's artifact
  identity, so replacing an implementation with the same interface leaves the
  agent artifact unchanged.
- **`tool_digest`** — the exact `.ktool` bytes plus the runtime image digest.
  It pins the deployed implementation in the lockfile.

Approvals and timestamps are **records keyed by `tool_digest`**, never hashed
into identity. Widening a toolset's capabilities changes `tool_digest` (so it
needs operator approval) but not `descriptor_digest` (so it needs no agent
rebuild). For a Service endpoint, verification compares the live `tools/list`
against **all** of the toolset's approved descriptors, so drift in any tool of
the endpoint fails the approved toolset.

### Manifest versions and the lockfile

An agent manifest lists tools as `bundle/local` or with a semver requirement on
the toolset version, `bundle/local@^1.2`. The builder resolves the requirement
to the **newest approved toolset version** satisfying it and pins
`version`, `requirement`, `tier`, `tool_digest`, and the per-tool
`descriptor_digest`s into `external_tools.lock.json` (lockfile v2). The lockfile
is always enforced at deploy; an operator-approved same-interface implementation
swap (a different `tool_digest` whose descriptors match the pinned
`descriptor_digest`s) is accepted and recorded in provenance.

See the `karakara-tool-authoring` skill for the full authoring walkthrough.


## Registry Architecture

The Karakara registry provides:

### MCP Snapshots
- **Server metadata** - Command, args, environment
- **Schema cache** - Tool/resource definitions
- **Version tracking** - Immutable snapshot references

### External Tool Snapshots (v2 toolsets)
- **Content-addressed identity** - `tool_digest` (exact `.ktool` bytes + runtime image) and per-tool `descriptor_digest` (interface)
- **Approval records** - keyed by `tool_digest`, carrying the toolset capability grant; never hashed into identity
- **Delivery** - `Bundled` (audited `.ktool` bytes) or `Reference` (a Service endpoint, verified against its live `tools/list`)

### Offline Capabilities
- **Snapshot export** - Bundle for CI/test environments
- **Cache validation** - Verify snapshot integrity
- **Fallback resolution** - Graceful degradation without registry

## Best Practices

### Tool Design
1. **Single responsibility** - One tool per logical capability
2. **Stateless operations** - Minimize session state complexity
3. **Error handling** - Provide clear error messages and codes
4. **Schema validation** - Validate inputs/outputs strictly

### Performance
1. **Lazy initialization** - Start tools only when needed
2. **Connection pooling** - Reuse tool sessions where possible
3. **Timeout handling** - Set reasonable operation timeouts
4. **Resource cleanup** - Ensure proper session termination

### Security
1. **Sandbox isolation** - Run tools in restricted environments
2. **Input validation** - Sanitize all tool inputs
3. **Capability limits** - Restrict tool access to necessary resources
4. **Audit logging** - Track tool usage and outcomes

## Troubleshooting

### Common Issues

**Tool not found**
- Verify tool is registered in registry
- Check agent's tool dependencies
- Ensure registry URL is accessible

**Schema validation errors**
- Compare the toolset's `tools/list` descriptors with BAML expectations
- Regenerate BAML types after tool changes
- Validate JSON schema syntax

**Session timeout**
- Check the MCP worker's responsiveness
- Verify the sandbox/VM resource limits
- Review tool operation complexity

**Registry connection issues**
- Confirm registry URL and authentication
- Test network connectivity
- Check snapshot cache availability

### Debug Commands

```bash
# Build a toolset: run its MCP server, read tools/list, assemble the .ktool
karak tool build ./tools/support

# Invoke one tool through the stdio DevBackend
karak tool test ./tools/support --tool support/clickup --input '{}'

# List tools from the runner/repository catalog or an offline snapshot cache
karak list-tools

# Workspace integrity check
karak doctor
```
