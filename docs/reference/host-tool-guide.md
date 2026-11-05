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
binding = "egress_bound"   # or "reveal"; required (see Secrets below)
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

### Secrets: how a key reaches your tool code

A tool's secret **never travels through MCP calls**. The MCP `tools/call`
between the supervisor and your server carries only arguments and results, the
same data the LLM chose and provenance records. Secrets take a separate route
that only the host controls.

Example: an external ClickUp toolset that needs an API token.

**1. The author declares the request** (`karakara.toml`). This grants nothing;
the operator decides.

```toml
[toolset]
name = "support"
version = "0.1.0"

[[toolset.capabilities.net]]
host = "api.clickup.com"
port = 443

[[toolset.capabilities.secrets]]
name = "CLICKUP_API_KEY"
binding = "egress_bound"   # ClickUp sends the token as a header
```

**2. The author writes ordinary code** with the official MCP SDK. There is no
Karakara secret API:

```ts
server.registerTool("create_task", { inputSchema: { list_id: z.string(), name: z.string() } },
  async ({ list_id, name }) => {
    const res = await fetch(`https://api.clickup.com/api/v2/list/${list_id}/task`, {
      method: "POST",
      headers: {
        // Inside the VM this is a placeholder ("$MSB_CLICKUP_API_KEY"),
        // never the real token.
        Authorization: process.env.CLICKUP_API_KEY!,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ name }),
    });
    const task = await res.json();
    return { structuredContent: task, content: [{ type: "text", text: JSON.stringify(task) }] };
  });
```

**3. The operator** puts the real token in fnox / the secret store and binds it
in policy: this agent may use this exact toolset digest, and
`CLICKUP_API_KEY` may only be sent to `api.clickup.com`.

**4. At runtime there are two routes, and they never mix:**

```
ROUTE 1 — the call: arguments and results only, never a secret
  LLM / agent ─"create_task {list_id, name}"─► runner ─protocol_v2 (vsock)─► supervisor ─MCP stdio─► tool
       ▲                                                                                              │
       └───────────────────────────────────── result ◄───────────────────────────────────────────────┘

ROUTE 2 — the secret: host side only
  fnox (real token) ─► runner ─► microsandbox network layer (host side)
                                    │ at VM create: env CLICKUP_API_KEY = "$MSB_CLICKUP_API_KEY"
                                    ▼
                             supervisor env ─► worker env ─► process.env in the tool (placeholder)

WHERE THEY MEET — on the network, outside the guest
  tool: fetch(api.clickup.com, Authorization: "$MSB_CLICKUP_API_KEY")
        │ TLS from the guest
        ▼
  host network layer: is this really api.clickup.com (DNS identity + verified TLS)?
        ├─ yes → swap the placeholder for the real token ─► api.clickup.com
        └─ no  → cut the connection
```

Why it works this way: everything on Route 1 is seen by the LLM (which can be
prompt-injected), recorded in provenance, and logged. Keeping the secret off
that route means the most a misbehaving tool or a tricked LLM can leak is the
placeholder, which is worthless anywhere except a verified TLS connection to
the bound host.

How the VM enforces it:

- The guest resolves the bound host through the VM's DNS forwarder; that answer
  is the identity the network layer checks before substituting. DNS answers
  only names the toolset requested, so nothing else resolves.
- Plain HTTP, or any other host, never receives the real value.
- Locally, `karak tool test` runs your server outside a VM, so you can export a
  real `CLICKUP_API_KEY` for development; the code does not change.

**Choosing a binding**

| Binding | Use when | What the tool sees |
|---|---|---|
| `egress_bound` (default choice) | the secret is sent as-is on the wire: `Authorization: Bearer …`, `x-api-key`, API-key headers | a placeholder in `process.env.<NAME>` |
| `reveal` (separately approved, rare) | the code must compute with the value: request signing (AWS SigV4, HMAC webhooks), non-HTTP protocols | the real value, delivered to that one worker over a sealed fd; `KARAKARA_REVEAL_FDS` maps `NAME` → fd number. Never in env, argv, or the invoke payload |

With `reveal`, the plaintext is inside the VM, in one worker only. Prefer
short-lived credentials (e.g. AWS STS) over long-lived keys.

Caveats:

- Header substitution applies to every request header. An API that echoes a
  request header back would return the real value to the tool. Send the
  placeholder only in the header the API expects.
- The real value also exists on the host: in fnox, the runner's memory, and
  today in microsandbox's local state (`$MSB_HOME`). Keep that directory on
  memory-backed storage in deployments, and run the runner as its own OS user
  when other agents share the machine.

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
