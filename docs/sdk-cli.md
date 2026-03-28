# Agentium OS SDK CLI

The `cargo-agent-platform` CLI automates scaffolding of new tools and agents, eliminating manual boilerplate while preserving the existing compile-time tool registration model.

## Quick Reference

| Command | Description |
|---------|-------------|
| `new-tool [name]` | Create a new tool crate with all necessary patches |
| `new-agent [name]` | Create a new agent package with templates |
| `build [names]...` | Package agents into distributable tar.gz files |
| `publish --agent-dir <path>` | Publish agent source bundle to repository |
| `deploy --hash <hash>` | Deploy a published content hash into a running runner |
| `undeploy --hash <hash>` | Remove an active deployment from a running runner |
| `list-deployed-instances` | List currently loaded runner agent instances |
| `list-tools` | List all registered tools from the inventory |
| `list-agents` | List all agent packages |
| `list-event-sources` | List event source kinds and known schema versions |
| `regen [names]...` | Regenerate type declarations for agents (all if omitted) |
| `doctor` | Validate workspace integrity |
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

## Commands

### `new-tool`

Creates a new tool crate with all necessary file patches. Omit `name` for interactive mode.

```bash
cargo agent-platform new-tool [name] [options]
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
cargo agent-platform new-tool github --dry-run
cargo agent-platform new-tool github --access write --description "GitHub REST API"
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

```bash
cargo agent-platform new-agent github-agent \
  --tools support/github,system/internal_a2a \
  --template planner \
  --description "GitHub issue and PR assistant"

cargo agent-platform new-agent intake-agent \
  --tools system/internal_a2a \
  --template planner \
  --subscriptions "schema=task-daemon.interpretation.v1,sources=slack,clickup"
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

---

### `publish`

Publishes an agent source directory to the repository. The repository assigns the version and content hash, builds the artifact, and stores it.

```bash
cargo agent-platform publish [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--agent-dir <path>` | `.` | Path to agent source directory (`manifest.json` + `baml_src/`) |
| `--repository-url <url>` | `http://127.0.0.1:8080/repository` | Repository base URL |
| `--rationale <text>` | `published from source directory` | Change rationale in publish metadata |
| `--origin <kind>` | `iteration` | `original` or `iteration` |

Tags are read from `manifest.json` — not accepted as a CLI flag.

```bash
cargo agent-platform publish --agent-dir ./agents/clickup-agent \
  --origin iteration --rationale "Improved task sync reliability"
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

### `deploy`

Deploys a previously published content hash into a running `baml-agent-runner`.

```bash
cargo agent-platform deploy [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--hash <sha256>` | *(required)* | Content hash returned by `publish` |
| `--url <base-url>` | `http://127.0.0.1:8080` | Runner base URL |

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
| `--url <base-url>` | `http://127.0.0.1:8080` | Runner base URL |

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
| `--url <base-url>` | `http://127.0.0.1:8080` | Runner base URL |

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

---

### `regen`

Regenerates `generated_tools.baml` and `baml-runtime.d.ts` for agents. Omit names to regenerate all.

```bash
cargo agent-platform regen [names]...
```

| Argument | Description |
|----------|-------------|
| `[names]...` | Agent names to regenerate (omit for all in `agents/` + `tests/fixtures/agents/`) |

Run after adding/modifying tools or BAML schemas, and before committing.

```bash
cargo agent-platform regen
cargo agent-platform regen clickup-agent notion-agent
```

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
| `--url <base-url>` | `http://127.0.0.1:8080` | Runner base URL |
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

---

## See Also

- `docs/host-to-agent-event-delivery.md` — Event delivery model and subscriptions
- `docs/agent-runner.md` — Running and managing the agent runner
- `CLAUDE.md` — Architecture overview and Rust conventions
- `agents/clickup-agent/` — Reference implementation for the `planner` template
- `agents/coordinator-agent/` — Reference implementation for the `coordinator` template
