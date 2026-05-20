# Agentium OS SDK CLI

The `cargo-agent-platform` CLI automates scaffolding of new tools and agents, eliminating manual boilerplate while preserving the existing compile-time tool registration model.

## Quick Reference

| Command | Description |
|---------|-------------|
| `new-tool [name]` | Create a standalone external tool scaffold (Rust/Bash/Python/TypeScript) — default path for most users |
| `new-static-tool [name]` | Create a compiled-in static tool crate with workspace patches — platform-internal use |
| `new-agent [name]` | Create a new agent package with templates |
| `build [names]...` | Package agents into distributable tar.gz files |
| `publish --agent-dir <path>` | Publish agent source bundle to repository |
| `deploy --hash <hash>` | Deploy a published content hash into a running runner |
| `undeploy --hash <hash>` | Remove an active deployment from a running runner |
| `list-deployed-instances` | List currently loaded runner agent instances |
| `list-tools` | List all registered tools from the inventory |
| `list-agents` | List all agent packages |
| `list-event-sources` | List event source kinds and known schema versions |
| `regen [names]... [--path <agent-dir>]` | Regenerate type declarations for agents (all if omitted) |
| `doctor` | Validate workspace integrity |
| `check-external-tool --path <dir>` | Validate external tool metadata against schema + runtime parser |
| `sandbox-bind-sync --tool-dir <dir> [--rootfs <path>] [--image <tag>]` | Generate local bind dev runtime lock + sidecar bundle; can build/export rootfs from Docker |
| `sandbox-oci-prepare --tool-dir <dir>` | Materialize OCI sidecar bundle from metadata (no registry pull required) |
| `mcp list` | List MCP servers imported into the repository registry |
| `mcp enable <server-id>` | Discover, approve, and store an MCP server snapshot in the repository registry |
| `mcp server|versions|tool ...` | Inspect MCP registry snapshots, versions, and platform tool entries |
| `chat --agent <name>` | Interactive terminal chat with a deployed agent |

## Installation

**Option 1 — run directly (recommended for development):**

```bash
cargo run -p cargo-agent-platform -- <command> [options]
```

**Option 2 — install to Cargo bin:**

```bash
cargo install --path crates/cargo-agent-platform
cargo agent-platform <command> [options]
```

**Option 3 — run the built binary directly:**

```bash
cargo build -p cargo-agent-platform
./target/debug/cargo-agent-platform <command> [options]
```

```bash
# Help
cargo agent-platform --help
cargo agent-platform <command> --help
```

---

## Operator Authentication

In cluster mode, operator actions (publish, deploy, undeploy, push, and MCP registry mutations such as `mcp enable`) require a runner token. The token is configured on the runner via `RUNNER_TOKEN` env or `--runner-token` flag (see `docs/agent-runner.md`).

The CLI resolves the token from two sources, in order of precedence:

1. **`--runner-token <token>`** — CLI flag (highest priority)
2. **`RUNNER_TOKEN`** — environment variable (fallback)

When neither is set, no authentication header is sent. This preserves backwards-compatible standalone/local operation where the runner has no token configured.

```bash
# Using the env variable (recommended for scripts)
export RUNNER_TOKEN="your-runner-token"
cargo agent-platform push --agents agents/my-agent --url https://runner.example.com

# Using the CLI flag
cargo agent-platform deploy --hash <sha256> --url https://runner.example.com \
  --runner-token "$RUNNER_TOKEN"
```

**Public commands** (`list-deployed-instances`, `chat`, and read-only MCP registry inspection commands) do not accept or require a token — they use public API routes.

---

## Commands

### `new-tool`

Creates a standalone external tool scaffold that speaks the V1 tool protocol over stdio. This is the default path for most users — the tool lives in its own directory and the runner picks it up at deploy time via `BAML_EXTERNAL_TOOLS_DIR`. Omit `name` for interactive mode (the prompt now includes runtime selection: `process` or `sandbox`).

```bash
cargo agent-platform new-tool [name] [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `name` | *(interactive)* | Tool local name in lowercase (`a-z0-9_-`), e.g. `echo`, `clickup_sync`, `clickup-sync` |
| `--bundle <bundle>` | `support` | Bundle namespace. Free-form (e.g. `support`, `travel`, `acme`). Must be non-empty and contain no `/`. Validated against `baml_rt_tools::BundleName` at scaffold and runtime. |
| `--lang <lang>` | `rust` | Scaffold language: `rust`, `bash`, `python`, `typescript` |
| `--access <level>` | `read` | `read` (query-only), `write` (create/update), or `delete` (strictest level) |
| `--runtime <kind>` | `process` | Metadata runtime: `process` or `sandbox` |
| `--invocation-mode <mode>` | `single-shot` | Invocation contract: `single-shot` or `session` (`session` requires `--runtime sandbox`) |
| `--sandbox-source <kind>` | `oci` | Sandbox image source: `oci` or `bind` |
| `--sandbox-image <ref@sha256:...>` | — | Required when `--runtime sandbox --sandbox-source oci` |
| `--sandbox-entrypoint <argv,...>` | — | Optional comma-separated entrypoint argv for sandbox runtime |
| `--generate-docker` | off | For `--runtime sandbox --sandbox-source bind`, also scaffold `adapter/Dockerfile` + Docker-assisted `setup_bind_sandbox.sh` |
| `--description <text>` | `""` | Human-readable description written into metadata |
| `--output <dir>` | `./<name>` | Output directory for standalone tool project |
| `--dry-run` | off | Preview changes without writing files (non-interactive only) |

Generated scaffold always includes `tool-metadata.json`, `README.md`, and language-specific files. For `runtime=process`, language templates include `tool-server`, which is the host executable the runner invokes. For `runtime=sandbox`, non-Bash scaffolds omit host `tool-server`; runner invocation goes through `/tool-adapter` inside the sandbox. Bash sandbox scaffolds still include `tool-server` because it is the Bash implementation script that the adapter delegates to. `tool-metadata.json` always emits an explicit `runtime` block (`process` by default, or `sandbox` when selected).

`--invocation-mode session` scaffolds metadata with `invocation_mode: "session"` (external session protocol). For now, scaffold defaults still keep session knobs conservative (`session_policy: strict`, `secret_scope: send`) unless you edit metadata manually.

For `sandbox + bind`, scaffolding emits a portable tool-relative rootfs path such as `./.tmp/<bundle>-<tool>-rootfs`. Bind is a local development convenience; host-specific absolute paths live in the gitignored `tool-metadata.lock.json` generated by `sandbox-bind-sync`.

Bind scaffold modes:
- default (no `--generate-docker`): metadata-only bind scaffold; materialize rootfs externally, then run `sandbox-bind-sync` to generate `tool-metadata.lock.json`, write the adapter sidecar bundle, and validate.
- with `--generate-docker`: additionally emits `adapter/Dockerfile` + `adapter/tool-adapter` + `setup_bind_sandbox.sh`; script builds image, exports rootfs, writes `tool-metadata.lock.json`, writes adapter sidecar bundle (`/etc/agentium/tool-bundle.json`), and validates metadata.

Bind caveat: bind-rootfs mode carries filesystem contents, not guaranteed OCI image config (e.g. Dockerfile `ENV`). Generated adapters should not require env vars for baseline startup.

For `setup_bind_sandbox.sh`, command resolution is:
1. `AGENT_PLATFORM_CMD` (if set),
2. `cargo agent-platform <subcommand>` (only if that subcommand exists),
3. `cargo run -q -p cargo-agent-platform -- <subcommand>` (workspace fallback).

Use `AGENT_PLATFORM_CMD` to avoid stale plugin mismatches outside this workspace, e.g.:

```bash
export AGENT_PLATFORM_CMD='cargo run -q -p cargo-agent-platform --'
```

```bash
cargo agent-platform new-tool echo --lang bash --output ./echo-tool
cargo agent-platform new-tool weather --lang typescript --access write
cargo agent-platform new-tool flight-search --lang rust --bundle travel
cargo agent-platform new-tool secure-devtool \
  --runtime sandbox \
  --sandbox-source oci \
  --sandbox-image ghcr.io/acme/secure-devtool@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --sandbox-entrypoint /app/tool-adapter

cargo agent-platform new-tool dev_echo \
  --runtime sandbox \
  --sandbox-source bind \
  --sandbox-entrypoint /tool-adapter

cargo agent-platform new-tool dev_echo_docker \
  --runtime sandbox \
  --sandbox-source bind \
  --generate-docker

cargo agent-platform new-tool streamed_echo \
  --runtime sandbox \
  --invocation-mode session \
  --sandbox-source oci \
  --sandbox-image ghcr.io/acme/streamed-echo@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

---

### `new-static-tool`

Creates a *static* tool crate — compiled into the platform workspace and linked
at build time. Use this only when extending the platform itself (e.g. adding
a system bundle); every other case should prefer `new-tool`, which produces a
standalone external scaffold. Omit `name` for interactive mode.

```bash
cargo agent-platform new-static-tool [name] [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `name` | *(interactive)* | Tool name in kebab-case (e.g. `github`, `jira`) |
| `--bundle <type>` | `support` | Bundle type — only `support` is currently supported |
| `--access <level>` | `read` | `read` (query-only) or `write` (can mutate) |
| `--description <text>` | `""` | Human-readable description shown in discovery UIs |
| `--dry-run` | off | Preview changes without writing files (non-interactive only) |

**What it creates:** `crates/tools/<name>/Cargo.toml` + `src/lib.rs`

**What it patches:** workspace `Cargo.toml`, `baml-tool-links`, `baml-agent-runner`, `baml-rt-builder` (Cargo deps, feature forwarding, and `force_link_all_tools!` entry).

> If your tool needs runtime context (agent name, manifest data), also manually edit `optional_tool_bundles.rs` in runner and builder. See the `memory` tool for an example.

```bash
cargo agent-platform new-static-tool github --dry-run
cargo agent-platform new-static-tool github --access write --description "GitHub REST API"
```

---

### `new-agent`

Creates a new agent package with generated BAML prompts, TypeScript entry point, and type declarations. Omit `name` for interactive mode.

```bash
cargo agent-platform new-agent [name] [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `name` | *(interactive)* | Agent name in kebab-case (e.g. `github-agent`) |
| `--tools <ids>` | — | Comma-separated tool IDs (e.g. `support/github,system/internal_a2a`) |
| `--template <name>` | `simple` | `simple`, `basic-tools`, `planner`, `coordinator` |
| `--description <text>` | `""` | Human-readable description for agent discovery |
| `--tags <tags>` | — | Comma-separated manifest tags (e.g. `support,clickup,prod`) |
| `--subscriptions <sub>` | — | Event subscriptions — format: `schema=<version>,sources=<kind1,kind2>` |
| `--output <dir>` | `agents/<name>` | Target directory for the agent package |
| `--dry-run` | off | Preview changes without writing files (non-interactive only) |

**Templates:**

| Template | Description |
|----------|-------------|
| `simple` | Basic agent without tools — Q&A, summarizers |
| `basic-tools` | Simple agent with tool support |
| `planner` | 3-phase: Intent → Plan → Execute (based on clickup-agent) |
| `coordinator` | Multi-agent delegator with DAG-based workflow execution |

**What it creates:** `agents/<name>/manifest.json`, `tsconfig.json`, `baml_src/<name>_prompt.baml`, `src/index.ts`, and generated type files.

> `--subscriptions` records manifest subscriptions for dispatch-capable agents. The `coordinator` template rejects subscriptions, and other templates may still require a manual `onDispatch` implementation before they can actually receive dispatched events.

```bash
cargo agent-platform new-agent github-agent \
  --tools support/github,system/internal_a2a \
  --template planner \
  --description "GitHub issue and PR assistant"

cargo agent-platform new-agent intake-agent \
  --tools system/internal_a2a \
  --template planner \
  --subscriptions "schema=host.source-records.v1,sources=slack"

cargo agent-platform new-agent callback-handler-agent \
  --tools system/internal_a2a \
  --template planner \
  --subscriptions "schema=system.callback.v1,sources=system/callback"
```

---

### `build`

Packages one or more agents into distributable tar.gz files. Omit names to build from the current directory.

```bash
cargo agent-platform build [names]... [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `[names]...` | *(current dir)* | Agent names to look up in `agents/` |
| `--path <path>` | — | Explicit agent directory path (single agent only) |
| `-o, --output <dir>` | `.` | Output directory for tar.gz files |

```bash
cargo agent-platform build clickup-agent notion-agent -o ./dist/
cargo agent-platform build --path agents/clickup-agent
```

Output: `<agent-name>-<version>.tar.gz`

If the agent manifest includes MCP tools (`mcp/<server>/<tool>`), set `BAML_MCP_REGISTRY_URL` to a repository URL so the builder resolves approved registry snapshots during type generation/package assembly:

```bash
BAML_MCP_REGISTRY_URL=http://127.0.0.1:18080/repository \
  cargo agent-platform build --path examples/agents/meteo-mcp-agent
```

---

### `publish`

Publishes an agent source directory to the repository. The repository assigns the version and content hash, builds the artifact, and stores it.

```bash
cargo agent-platform publish [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--agent-dir <path>` | `.` | Path to agent source directory (`manifest.json` + `baml_src/`) |
| `--repository-url <url>` | `http://127.0.0.1:18080/repository` | Repository base URL |
| `--rationale <text>` | `published from source directory` | Change rationale in publish metadata |
| `--origin <kind>` | `iteration` | `original` or `iteration` |
| `--runner-token <token>` | `RUNNER_TOKEN` env | Operator token for authenticated access |

Tags are read from `manifest.json` — not accepted as a CLI flag.

```bash
# Publish current directory
cargo agent-platform publish --agent-dir .

# Publish to another repository URL
cargo agent-platform publish \
  --agent-dir ./agents/notion-agent \
  --repository-url http://127.0.0.1:18080/repository

# Publish as an iteration with explicit rationale
cargo agent-platform publish \
  --agent-dir ./agents/clickup-agent \
  --origin iteration \
  --rationale "Improved task sync reliability"
```

Example output:
```
Source published successfully.
  agent dir: agents/clickup-agent
  version:   clickup-agent@v3
  hash:      8b0d0973de403b3b32e9ff234d5b996b8250d9708f6f09b54178c843f19cde5c
```

Use the returned `hash` with `deploy`.

---

### `push`

Publishes and deploys one or more agent source directories sequentially.

Behavior notes:
- Duplicate agent paths in `--agents` are skipped (first occurrence wins).
- A preflight validation runs before any network call (`path`, `manifest.json`, `baml_src/`).
- Execution continues across per-agent publish/deploy failures and prints a final report.
- Command exits non-zero if any agent failed.

```bash
cargo agent-platform push [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--agents <path...>` | *(required)* | Agent source directories; accepts comma-separated and/or space-separated values |
| `--repository-url <url>` | `http://127.0.0.1:18080/repository` | Repository base URL |
| `--rationale <text>` | `published from source directory` | Change rationale in publish metadata |
| `--origin <kind>` | `iteration` | `original` or `iteration` |
| `--url <base-url>` | `http://127.0.0.1:18080` | Runner base URL for deploy |
| `--runner-token <token>` | `RUNNER_TOKEN` env | Operator token for authenticated access |

```bash
# Comma-separated
cargo agent-platform push \
  --agents agents/clickup-agent,agents/notion-agent,agents/coordinator-agent

# Space-separated
cargo agent-platform push \
  --agents agents/clickup-agent agents/notion-agent agents/coordinator-agent
```

---

### `deploy`

Deploys a previously published content hash into a running `baml-agent-runner`.

```bash
cargo agent-platform deploy [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--hash <sha256>` | *(required)* | Content hash returned by `publish` |
| `--url <base-url>` | `http://127.0.0.1:18080` | Runner base URL |
| `--runner-token <token>` | `RUNNER_TOKEN` env | Operator token for authenticated access |

```bash
cargo agent-platform deploy --hash bfe72df219673c1a919817b29c37c2b51419e1e81b61eeca5e5549bd7b1b5d83
```

---

### `undeploy`

Removes an active package hash from a running `baml-agent-runner`.

```bash
cargo agent-platform undeploy [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--hash <sha256>` | *(required)* | Content hash of the deployed package |
| `--url <base-url>` | `http://127.0.0.1:18080` | Runner base URL |
| `--runner-token <token>` | `RUNNER_TOKEN` env | Operator token for authenticated access |

```bash
cargo agent-platform undeploy --hash bfe72df219673c1a919817b29c37c2b51419e1e81b61eeca5e5549bd7b1b5d83
```

---

### `list-deployed-instances`

Lists currently loaded agent instances from a running runner (`GET /agents`).

```bash
cargo agent-platform list-deployed-instances [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--url <base-url>` | `http://127.0.0.1:18080` | Runner base URL |

---

### `list-tools`

Lists all registered tools from the compiled inventory.

```bash
cargo agent-platform list-tools
```

Output columns: `NAME`, `DESCRIPTION` (truncated), `TAGS`, `ACCESS` (Read/Write/None).

---

### `list-agents`

Lists all agent packages found in `agents/` and `tests/fixtures/agents/`.

```bash
cargo agent-platform list-agents
```

Output columns: `NAME`, `VERSION`, `SOURCE` (production/fixture), `DESCRIPTION` (truncated).

---

### `list-event-sources`

Lists event source kinds declared by tools and known schema versions for agent subscriptions.

```bash
cargo agent-platform list-event-sources
```

Use this to discover available source kinds and schema versions before creating agents with `--subscriptions`.

Current built-in schema surfaces include:

- `host.source-records.v1` for raw host-managed source ingress
- `system.callback.v1` for durable host-native callback delivery
- `host.source-records.v1` from task-daemon via `POST /events/publish`

---

### `regen`

Regenerates `_baml_runtime.baml` and `baml-runtime.d.ts` for agents.

```bash
cargo agent-platform regen [names]... [--path <agent-dir>]
```

| Argument / Option | Description |
|-------------------|-------------|
| `[names]...` | Agent names to regenerate (omit for all in `agents/` + `tests/fixtures/agents/`) |
| `--path <agent-dir>` | Explicit agent directory path (repeat flag for multiple paths) |

Notes:
- `--path` cannot be combined with agent names.
- If the agent uses external tools, set `BAML_EXTERNAL_TOOLS_DIR` to one or more tool directories containing `tool-metadata.json` (colon-separated).
- If the agent uses MCP tools, set `BAML_MCP_REGISTRY_URL` to the repository URL so generated BAML/TypeScript uses approved registry snapshots.

Run after adding/modifying tools or BAML schemas, and before committing.

```bash
cargo agent-platform regen
cargo agent-platform regen clickup-agent notion-agent
cargo agent-platform regen --path examples/agents/echo-agent
```

---

### `check-external-tool`

Validates a standalone external tool's `tool-metadata.json` against:

1. `schemas/external_tool_metadata.schema.json`
2. The runtime typed parser (`ExternalToolMetadata`)
3. Sandbox source consistency checks:
   - `oci`: image must be digest-pinned (`repo@sha256:...`)
   - `bind`: source metadata must be portable and the lock sidecar, when present, provides the resolved local path

```bash
cargo agent-platform check-external-tool --path ./echo-tool
```

| Option | Default | Description |
|--------|---------|-------------|
| `--path <dir>` | `.` | Tool directory containing `tool-metadata.json` |

---

### `sandbox-bind-sync`

Synchronizes a local-development bind sandbox rootfs with runtime state. It never mutates committed `tool-metadata.json`; instead it writes the gitignored sibling `tool-metadata.lock.json` with the host-resolved rootfs path and writes the in-rootfs adapter sidecar at `/etc/agentium/tool-bundle.json`.

```bash
# Existing rootfs mode: --rootfs defaults to runtime.image.path from metadata.
cargo agent-platform sandbox-bind-sync \
  --tool-dir ./examples/external-tools/claude-ext \
  --check

# Docker-assisted mode: image remains explicit; Dockerfile defaults to adapter/Dockerfile.
cargo agent-platform sandbox-bind-sync \
  --tool-dir ./examples/external-tools/claude-ext \
  --image dev-claude-ext-sandbox:local \
  --force \
  --check
```

| Option | Default | Description |
|--------|---------|-------------|
| `--tool-dir <dir>` | *(required)* | Tool directory containing `tool-metadata.json` |
| `--rootfs <path>` | `runtime.image.path` | Bind rootfs directory. Relative paths resolve against `--tool-dir` |
| `--image <tag>` | — | Explicit local Docker image tag/name to build/export from |
| `--dockerfile <path>` | `adapter/Dockerfile` when `--image` is set | Dockerfile for Docker-assisted mode. Relative paths resolve against `--tool-dir` |
| `--force` | off | Recreate the rootfs directory if it already exists |
| `--check` | off | Run `check-external-tool` after writing lock + sidecar |
| `--dry-run` | off | Validate and print planned values without writing files |
| `--json` | off | Emit machine-readable JSON summary |

`--image` is intentionally explicit: scaffolds and setup scripts use the conventional `<bundle>-<tool-name>-sandbox:local` tag, but the sync command should not silently pick or overwrite a Docker tag if the caller did not ask for Docker-assisted mode.

---

### `mcp`

Inspects and manages approved MCP server snapshots in the repository registry. MCP tools are exposed to agents as concrete platform tool names such as `mcp/meteo/get_meteo`; the registry stores the approved server snapshot and per-tool schema digests used by builder code generation.

```bash
cargo agent-platform mcp <command> [options]
```

| Subcommand | Description |
|------------|-------------|
| `list` | List MCP servers known to the repository registry |
| `enable <server-id>` | Discover `initialize` + `tools/list`, prompt for approval, and import the approved snapshot into the registry |
| `server <server-id> [--version <n>]` | Show the latest or a specific server snapshot summary |
| `versions <server-id>` | List immutable registry versions for a server |
| `tool <platform-tool-name>` | Lookup registry entries by platform tool name, e.g. `mcp/meteo/get_meteo` |

Common options:

| Option | Default | Description |
|--------|---------|-------------|
| `--repository-url <url>` | `http://127.0.0.1:18080/repository` | Repository base URL where `/repository/*` routes are mounted |
| `--json` | off | Emit raw JSON for read-only inspection commands |
| `--runner-token <token>` | `RUNNER_TOKEN` env | Operator token for mutation commands (`enable`) |

`enable` also accepts `--config <path>` (default: `$HOME/.agentium-os/mcp-servers.json`) and `--yes` to skip the interactive approval prompt.

Example local registry flow:

```bash
# Start a runner exposing /repository first.
cargo run -p baml-agent-runner --all-features -- --serve-http 127.0.0.1:18080

# Import and approve a local stdio MCP server declared in mcp-servers.json.
cargo agent-platform mcp enable meteo \
  --config ~/.agentium-os/mcp-servers.json \
  --repository-url http://127.0.0.1:18080/repository \
  --yes

# Review registry state.
cargo agent-platform mcp list
cargo agent-platform mcp versions meteo
cargo agent-platform mcp server meteo
cargo agent-platform mcp tool mcp/meteo/get_meteo
```

Build/regen note: when an agent manifest includes `mcp/<server>/<tool>` entries, type generation resolves the server snapshot from the registry when `BAML_MCP_REGISTRY_URL` is set. Registry snapshots resolved during build are packaged under `mcp/` for runtime compatibility.

```bash
BAML_MCP_REGISTRY_URL=http://127.0.0.1:18080/repository \
  cargo agent-platform build --path examples/agents/meteo-mcp-agent
```

The `cargo agent-platform mcp enable` command uses the builder library directly. It does not require a separate `baml-agent-builder` binary.

Compatibility builder command:

```bash
baml-agent-builder mcp-registry-enable <server-id> [--config <path>] [--repository-url <url>] [--yes] [--runner-token <token>]
```

The repository registry is the source of truth. Packaged MCP snapshot files are build artifacts derived from registry versions, not an operator-managed `~/.agentium-os/mcp` cache.

---

### `doctor`

Validates workspace integrity across two layers:

1. **Static checks** — tool crates in workspace members, `baml-tool-links` deps, `force_link_all_tools!` entries, feature forwarding in runner/builder.
2. **Catalog checks** — agent manifests reference tools that exist in the compiled inventory.

```bash
cargo agent-platform doctor [options]
```

| Option | Description |
|--------|-------------|
| `--ci` | Exit non-zero on any issue (for CI pipelines) |
| `--warn-missing-catalog` | Downgrade missing catalog entries from error to warning |

```bash
cargo agent-platform doctor --ci
```

---

### `chat`

Interactive terminal chat with a deployed agent. Discovers available agents via `GET /agents`, validates the target, then opens a REPL loop sending JSON-RPC `message.sendStream` requests.

```bash
cargo agent-platform chat --agent <name> [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--agent <name>` | *(required)* | Agent name or package from discovery |
| `--url <base-url>` | `http://127.0.0.1:18080` | Runner base URL |
| `--instance <id>` | `default` | Agent instance identifier |
| `-v, --verbose` | off | Print debug info (message IDs, context IDs, raw errors) |

Type your message and press Enter. Exit with `quit`, `exit`, `/quit`, `/exit`, or Ctrl+D.

```bash
cargo agent-platform chat --agent clickup-agent
cargo agent-platform chat --agent clickup-agent --instance default --verbose
```

---

## CI Integration

```yaml
- name: Regenerate type declarations
  run: cargo run -p cargo-agent-platform -- regen

- name: Check for stale generated files
  run: |
    if ! git diff --quiet -- 'agents/' 'tests/fixtures/agents/'; then
      echo "::error::Generated files are stale. Run 'cargo agent-platform regen' and commit."
      exit 1
    fi

- name: Workspace integrity check
  run: cargo run -p cargo-agent-platform -- doctor --ci
```

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `Tool already exists` | Choose a different name or delete the failed crate directory |
| Patches fail to apply | Patches are transactional — all rolled back on failure. Run `doctor` to diagnose. |
| Tool not in `list-tools` | Check it compiles, is in `force_link_all_tools!`, and the feature is enabled |
| Agent not in `list-agents` | Ensure `manifest.json` exists with valid `name`, `version`, `entry_point` fields |
| `new-agent` fails with "Directory already exists" | Choose a different name/output or delete the existing directory |
| `regen` fails for an agent | Check for missing/invalid `baml_src/`, BAML syntax errors, or missing `manifest.json` |
| `chat` fails "Agent target not found" | Run `list-deployed-instances` to see valid agent/instance combinations |
| "authentication required" on publish/deploy/undeploy | Runner is in cluster mode — pass `--runner-token` or set `RUNNER_TOKEN` |
| "runner token was rejected" | Token does not match the server's `RUNNER_TOKEN` — verify the value |
| "Runner token is empty or whitespace-only" | An empty string was passed via `--runner-token` or `RUNNER_TOKEN` |

---

## See Also

- `docs/host-to-agent-event-delivery.md` — Event delivery model and subscriptions
- `docs/agent-runner.md` — Running and managing the agent runner
- `CLAUDE.md` — Architecture overview and Rust conventions
- `agents/clickup-agent/` — Reference implementation for the `planner` template
- `agents/coordinator-agent/` — Reference implementation for the `coordinator` template
