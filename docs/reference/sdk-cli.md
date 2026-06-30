# SDK CLI Reference

The `cargo-agent-platform` CLI provides commands for agent development, packaging, deployment, and external tool management.

## Installation

```bash
cargo install --path crates/cargo-agent-platform
```

Or use the alias:

```bash
alias builder='cargo run --bin cargo-agent-platform --'
```

## Core Commands

### Agent Development

#### `scaffold`
Create a new agent project from templates.

```bash
builder scaffold --name my-agent --template basic
```

**Options:**
- `--name` - Agent name
- `--template` - Template type (basic, advanced, etc.)
- `--output-dir` - Target directory (default: current)

#### `bootstrap` (`baml-agent-builder`)

Scaffold a new agent package with manifest, BAML prompt stub, and `src/index.ts`:

```bash
cargo run -p baml-rt-builder --bin baml-agent-builder -- bootstrap ./my-agent \
  --name "My Agent" \
  --description "Short description" \
  --no-tools
```

Then `lint`, customize `src/index.ts` / `baml_src/`, and `publish`. Deterministic eval smoke: [`verify-bootstrap-eval.sh`](../../scripts/verify-bootstrap-eval.sh) and fixture [`bootstrap-echo-eval`](../../tests/fixtures/agents/bootstrap-echo-eval/).

#### `chat`
Interactive chat session with an agent.

```bash
builder chat --agent-dir ./my-agent
```

**Options:**
- `--agent-dir` - Path to agent directory
- `--model` - Override LLM model
- `--conversation-id` - Resume existing conversation

#### `regen`
Regenerate BAML types and bindings for agents.

```bash
builder regen --names agent1,agent2
builder regen --path ./agents/my-agent
```

**Options:**
- `--names` - Comma-separated agent names
- `--path` - Explicit agent directory paths (repeatable)
- `--repository-url` - Repository URL for MCP/external-tool snapshots (default: `http://127.0.0.1:18080/repository`)
- `--snapshot-cache` - Explicit read-only snapshot cache for offline CI/test regen

### Agent Packaging & Deployment

#### `build`
Package agents into deployable tar.gz bundles.

```bash
builder build --names agent1,agent2 --output ./dist
```

**Options:**
- `--names` - Comma-separated agent names to build
- `--path` - Explicit agent directory path
- `--output` - Output directory for tar.gz files (default: current directory)
- `--repository-url` - Repository URL for MCP/external-tool snapshots (default: `http://127.0.0.1:18080/repository`)
- `--snapshot-cache` - Explicit read-only snapshot cache for offline CI/test builds

#### `publish`
Publish agent source bundle to repository.

```bash
builder publish --agent-dir ./my-agent --repository-url http://localhost:18080/repository
```

**Options:**
- `--agent-dir` - Path to agent directory
- `--repository-url` - Target repository URL
- `--runner-token` - Authentication token (or use `RUNNER_TOKEN` env var)

#### `deploy`
Deploy agent to Kubernetes cluster.

```bash
builder deploy --agent-name my-agent --cluster-endpoint ws://localhost:8080/ws
```

**Options:**
- `--agent-name` - Name of agent to deploy
- `--cluster-endpoint` - WebSocket endpoint for cluster
- `--runner-token` - Authentication token
- `--repository-url` - Repository URL for agent packages

### External Tool Management

#### `external-tool enable`
Discover, approve, and import an external-tool snapshot into registry.

```bash
builder external-tool enable ./tools/weather-tool --repository-url http://localhost:18080/repository
```

**Options:**
- `dir` - Path to external tool directory (contains `tool-manifest.json`)
- `--repository-url` - Repository URL to import approved snapshot into
- `--runner-token` - Operator token for registry import (or use `RUNNER_TOKEN` env var)
- `--sandbox-rootfs` - Custom sandbox rootfs path
- `--bind-sandbox` - Generate sandbox binding code
- `--yes` - Skip confirmation prompts
- `--json` - Output JSON format

#### `external-tool refresh`
Refresh an existing external tool snapshot.

```bash
builder external-tool refresh weather-tool --dir ./tools/weather-tool
```

**Options:**
- `name` - Tool name (e.g., `support/weather`)
- `--dir` - Path to external tool directory
- `--repository-url` - Repository URL to import approved snapshot into
- `--runner-token` - Operator token for registry import
- `--yes` - Skip confirmation prompts
- `--json` - Output JSON format

#### `external-tool inspect`
Inspect external tool snapshot details.

```bash
builder external-tool inspect support/weather --json
```

**Options:**
- `name` - Tool name (e.g., `support/weather`)
- `--cache-dir` - Legacy local snapshot cache root for inspect/offline workflows
- `--json` - Output JSON format

### Validation & Diagnostics

#### `check-external-tool`
Validate standalone external tool manifest.

```bash
builder check-external-tool ./tools/weather-tool
```

#### `doctor`
Validate workspace integrity.

```bash
builder doctor --ci
```

**Options:**
- `--ci` - Exit non-zero on any issue (for CI)
- `--warn-missing-catalog` - Warn about missing MCP catalog

#### `snapshot-report`
Report contents of an explicit exported snapshot cache for offline CI.

```bash
builder snapshot-report --snapshot-cache ./cache --json
```

**Options:**
- `--snapshot-cache` - Snapshot cache root (may contain `mcp/` and `external-tools/`)
- `--json` - Emit raw JSON

### Repository Management

#### `list-agents`
List available agents in repository.

```bash
builder list-agents --repository-url http://localhost:18080/repository --json
```

#### `list-deployed-instances`
List deployed agent instances.

```bash
builder list-deployed-instances --url ws://localhost:8080/ws
```

## Environment Variables

- `RUNNER_TOKEN` - Default authentication token for repository operations
- `BAML_REGISTRY_URL` - Default repository URL for MCP/external-tool resolution
- `BAML_TEST_MODEL` - LLM model for testing (default: `x-ai/grok-4.3`)

## Configuration Files

### Agent Configuration
Each agent directory should contain:
- `baml_src/` - BAML source files
- `src/` - TypeScript/JavaScript source
- `package.json` - Node.js dependencies
- `tsconfig.json` - TypeScript configuration

### External Tool Configuration
External tool directories should contain:
- `tool-manifest.json` - Tool metadata and interface definition
- Implementation files (language-specific)

## Registry Integration

The CLI integrates with the Agentium registry for:
- **MCP server snapshots** - Cached MCP server configurations and schemas
- **External tool snapshots** - Validated external tool implementations
- **Agent packages** - Deployable agent bundles

Registry-first resolution ensures consistent, offline-capable builds with approved snapshots.

## Offline Workflows

For CI/test environments without registry access:
1. Export snapshot cache: `--snapshot-cache` option
2. Use in offline builds: `builder build --snapshot-cache ./exported-cache`
3. Validate cache contents: `builder snapshot-report --snapshot-cache ./cache`

## Examples

### Complete Agent Workflow
```bash
# Create new agent
builder scaffold --name weather-agent --template basic

# Develop and test
builder chat --agent-dir ./weather-agent

# Add external tool
builder external-tool enable ./tools/weather-api

# Regenerate types
builder regen --path ./weather-agent

# Package for deployment
builder build --names weather-agent --output ./dist

# Deploy to cluster
builder deploy --agent-name weather-agent --cluster-endpoint ws://prod.example.com/ws
```

### CI/CD Integration
```bash
# Validate workspace
builder doctor --ci

# Build with offline cache
builder build --names all --snapshot-cache ./ci-cache

# Publish to staging
builder publish --agent-dir ./my-agent --repository-url http://staging.repo.com
```
