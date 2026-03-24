# Agentium OS SDK CLI

The `cargo-agent-platform` CLI automates scaffolding of new tools and agents, eliminating manual boilerplate while preserving the existing compile-time tool registration model.

## Quick Reference

| Command | Description |
|---------|-------------|
| `new-tool <name>` | Create a new tool crate with all necessary patches |
| `new-agent <name>` | Create a new agent package with templates |
| `build [names]...` | Package agents into distributable tar.gz files |
| `list-tools` | List all registered tools from the inventory |
| `list-agents` | List all agent packages |
| `list-event-sources` | List event source kinds declared by tools and known schema versions |
| `regen [names]...` | Regenerate type declarations for agents (all if no names given) |
| `doctor` | Validate workspace integrity |

## Installation

### Option 1: Use without installation (recommended for development)

Run directly via `cargo run`:

```bash
cargo run -p cargo-agent-platform -- <command> [options]
```

Examples:
```bash
cargo run -p cargo-agent-platform -- list-tools
cargo run -p cargo-agent-platform -- new-tool github --dry-run
cargo run -p cargo-agent-platform -- new-agent my-agent --template planner
cargo run -p cargo-agent-platform -- doctor
```

### Option 2: Install locally

Install the binary to your Cargo bin directory:

```bash
cargo install --path crates/cargo-agent-platform
```

After installation, use the standard Cargo subcommand syntax:

```bash
cargo agent-platform list-tools
cargo agent-platform new-tool github
cargo agent-platform doctor
```

### Getting Help

```bash
# Show all commands
cargo agent-platform --help

# Show help for a specific command
cargo agent-platform new-tool --help
cargo agent-platform doctor --help
```

### Option 3: Run the binary directly

After building:

```bash
cargo build -p cargo-agent-platform
./target/debug/cargo-agent-platform list-tools
```

---

## Commands

### `list-tools`

Lists all registered tools from the inventory with their metadata.

```bash
cargo run -p cargo-agent-platform -- list-tools
```

**Output includes:**
- Tool name (e.g., `support/github`, `system/internal_a2a`)
- Description (truncated)
- Tags
- Access level (Read, Write, or None)

**Example output:**
```
NAME                           DESCRIPTION                                        TAGS                      ACCESS
claude/dev                     Host-managed Claude streaming session. Open o...   [claude, stream, session] Write
memory/add                     Store cognitive events (facts, decisions, inf...   [memory]                  Write
support/calculate              Performs mathematical calculations. Can handl...   [support, calculate]      None
support/clickup                Interact with ClickUp: navigate workspaces (t...   [support, clickup]        None
system/internal_a2a            Opens a conversational session to another age...   [system, a2a]             None

Total: 20 tool(s) registered
```

---

### `list-event-sources`

Lists event source kinds declared by tools and known schema versions for event delivery.

```bash
cargo run -p cargo-agent-platform -- list-event-sources
```

**Output includes:**
- Event source kinds declared by tools via `#[baml_tool(..., event_sources = ["kind"])]`
- Known schema versions that agents can subscribe to (currently hardcoded)

**Example output:**
```
Event Source Kinds (declared by tools):

  SOURCE KIND          TOOL                                DESCRIPTION
  weather              internal-dev/get_weather            Test weather tool

Known Schema Versions:

  SCHEMA VERSION                           DESCRIPTION
  task-daemon.interpretation.v1            Task daemon event interpretation (Slack, ClickUp, GitHub Issues)

Note: Schema versions are conventions defined by event producers (e.g., task-daemon).
      Use these in agent manifest subscriptions to receive matching events.

Total: 1 event source kind(s), 1 known schema version(s)
```

**Use cases:**
- Discover which tools can produce events when polled
- Find available schema versions for agent subscriptions
- Understand the event delivery model before creating agents that need to receive events

**Note:** The known schema versions are hardcoded. When new event producers are added, update the `KNOWN_SCHEMA_VERSIONS` constant in `list_event_sources.rs`.

---

### `new-tool`

Creates a new tool crate with all necessary file patches for it to compile and be discoverable.

**Interactive Mode (recommended):**

```bash
cargo run -p cargo-agent-platform -- new-tool
```

When no name is provided, interactive mode guides you through the process:

```
? Tool name (kebab-case): github
? Bundle type: support (default) - Standard support tool
? Access level: read (default) - Query-only, no side effects
? Description (optional): basic github API interactions

Summary:
  Name:   github
  Bundle: support
  Access: read
  Desc:   basic github API interactions

Operations to perform:
  CREATE DIR  crates/tools/github
  CREATE DIR  crates/tools/github/src
  CREATE      crates/tools/github/Cargo.toml
  CREATE      crates/tools/github/src/lib.rs
  EDIT        Cargo.toml
  EDIT        crates/baml-tool-links/Cargo.toml
  EDIT        crates/baml-tool-links/src/lib.rs
  EDIT        crates/baml-agent-runner/Cargo.toml
  EDIT        crates/baml-agent-runner/src/main.rs
  EDIT        crates/baml-rt-builder/Cargo.toml
  EDIT        crates/baml-rt-builder/src/baml-agent-builder.rs
  EDIT        crates/baml-rt-builder/src/bin/regen_fixtures.rs

Note:
  Most tools are auto-registered via inventory and need no extra setup.
  If your tool requires runtime context (e.g., agent name, manifest data),
  you'll also need to manually edit:
    - crates/baml-agent-runner/src/optional_tool_bundles.rs
    - crates/baml-rt-builder/src/optional_tool_bundles.rs
  See the memory tool for an example.

? Proceed? (Y/n):
```

**Non-Interactive Mode:**

```bash
cargo run -p cargo-agent-platform -- new-tool <name> [options]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<name>` | Tool name in kebab-case (e.g., `github`, `jira`, `linear`). Omit for interactive mode. |

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--bundle <type>` | `support` | Bundle type. Currently only `support` is supported. |
| `--access <level>` | `read` | Access level: `read` (query-only) or `write` (can mutate) |
| `--description <desc>` | `""` | Optional user-facing tool description shown in discovery and picker UIs |
| `--dry-run` | - | Validate and preview changes without writing files (non-interactive only). Returns non-zero if validation fails. |

**Access Levels:**

| Level | Description |
|-------|-------------|
| `read` | Query-only, no side effects. Default for most tools. |
| `write` | Can mutate data (create, update, delete). Use for tools that modify external state. |

**Examples:**

```bash
# Interactive mode (recommended for new users)
cargo run -p cargo-agent-platform -- new-tool

# Preview what would be created (non-interactive)
cargo run -p cargo-agent-platform -- new-tool github --dry-run

# Create a read-only tool (non-interactive with defaults)
cargo run -p cargo-agent-platform -- new-tool github

# Create a tool with write access (non-interactive)
cargo run -p cargo-agent-platform -- new-tool github --access write
```

**What it creates:**

```
crates/tools/<name>/
  Cargo.toml
  src/
    lib.rs
```

**What it patches:**

| File | Change |
|------|--------|
| `Cargo.toml` (workspace root) | Adds crate to `workspace.members` |
| `crates/baml-tool-links/Cargo.toml` | Adds optional dependency and feature |
| `crates/baml-tool-links/src/lib.rs` | Adds entry to `force_link_all_tools!` macro |
| `crates/baml-agent-runner/Cargo.toml` | Adds feature forwarding |
| `crates/baml-agent-runner/src/main.rs` | Adds force-link import for runtime tool registration |
| `crates/baml-rt-builder/Cargo.toml` | Adds feature forwarding |
| `crates/baml-rt-builder/src/baml-agent-builder.rs` | Adds force-link import for packaging/type generation |
| `crates/baml-rt-builder/src/bin/regen_fixtures.rs` | Adds force-link import for regen flows |

**After creating a tool:**

1. Edit `crates/tools/<name>/src/lib.rs` to implement your tool logic
2. Run `cargo check -p baml-tools-<name>` to verify compilation
3. Run `cargo run -p baml-rt-builder --bin regen_fixtures` to update generated files

**Tools requiring runtime context:**

Most tools are auto-registered via inventory and need no extra setup. However, if your tool requires runtime context (e.g., agent name, manifest data), you'll need to manually add initialization code to:

- `crates/baml-agent-runner/src/optional_tool_bundles.rs`
- `crates/baml-rt-builder/src/optional_tool_bundles.rs`

See the `memory` tool for an example of runtime initialization.

**Validation rules:**

- Name must be kebab-case (lowercase letters, numbers, hyphens)
- Name cannot start or end with a hyphen
- Name cannot contain consecutive hyphens
- Reserved names: `test`, `lib`, `bin`, `build`, `dev`

---

### `doctor`

Validates workspace integrity with static checks and catalog validation.

```bash
cargo run -p cargo-agent-platform -- doctor [options]
```

**Options:**

| Option | Description |
|--------|-------------|
| `--ci` | Exit non-zero on any issue (for CI pipelines) |
| `--warn-missing-catalog` | Downgrade missing catalog entries from error to warning |

**Layer 1 - Static checks (file-based):**

1. Every tool crate in `crates/tools/` is listed in workspace `Cargo.toml` members
2. Every tool crate has a matching dependency in `crates/baml-tool-links/Cargo.toml`
3. Every tool crate has an entry in `force_link_all_tools!` macro
4. Feature forwarding is configured in runner and builder `Cargo.toml`

**Layer 2 - Catalog checks (requires compiled inventory):**

5. Agent manifests reference tools that exist in the compiled inventory

**Example output:**

```
Found workspace root: /path/to/agent-platform

Layer 1: Static checks
  ✓ clickup in workspace members
  ✓ memory in workspace members
  ✓ calculator in workspace members
  ...
  Found 8 tool crate(s)
  ✓ clickup in baml-tool-links deps
  ✓ clickup in force_link_all_tools! macro
  ✓ clickup feature forwarding in runner
  ✓ clickup feature forwarding in builder
  ...

Layer 2: Catalog checks
  Found 20 tools in inventory

✓ All checks passed!
```

**CI usage:**

```bash
cargo run -p cargo-agent-platform -- doctor --ci
```

This exits with code 1 if any issues are found, suitable for CI pipelines.

---

### `new-agent`

Creates a new agent package with generated BAML prompts, TypeScript entry point, and type declarations.

**Interactive Mode (recommended):**

```bash
cargo run -p cargo-agent-platform -- new-agent
```

When no name is provided, interactive mode guides you through the process:

```
? Agent name: intake-agent
? Description (optional): Processes events from external sources
? Template: planner - 3-phase: Intent -> Plan -> Execute
? Select tools (Space to select, Enter to confirm):
  [x] system/internal_a2a
  [ ] system/discover_agents
  [ ] support/clickup

? Does this agent need to receive events? Yes
? Select schema versions to subscribe to:
  [x] task-daemon.interpretation.v1
? Select source kinds to subscribe to:
  [x] slack                (common)
  [x] clickup              (common)
  [ ] github_issues        (common)

Summary:
  Name:        intake-agent
  Template:    planner
  Description: Processes events from external sources
  Tools:       system/internal_a2a
  Subscriptions:
    Schemas: task-daemon.interpretation.v1
    Sources: slack, clickup
  Output:      /path/to/agents/intake-agent

Files to be created:
  agents/intake-agent/
    manifest.json
    tsconfig.json
    baml_src/
      intake_agent_prompt.baml
      generated_tools.baml (after type generation)
    src/
      index.ts
      baml-runtime.d.ts (after type generation)

? Proceed? (Y/n):
```

**Non-Interactive Mode:**

```bash
cargo run -p cargo-agent-platform -- new-agent <name> [options]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<name>` | Agent name in kebab-case (e.g., `github-agent`, `task-manager`). Omit for interactive mode. |

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--tools <tools>` | - | Comma-separated tool IDs (e.g., `support/github,system/internal_a2a`) |
| `--template <template>` | `simple` | Agent template: `simple`, `basic-tools`, `planner`, `coordinator` |
| `--description <desc>` | `""` | Human-readable description for agent discovery |
| `--subscriptions <sub>` | - | Event subscriptions (see Event Subscriptions below) |
| `--output <dir>` | `agents/<name>` | Target directory for the agent package |
| `--dry-run` | - | Validate and preview changes without writing files (non-interactive only). Returns non-zero if validation fails. |

**Templates:**

| Template | Description | Use Case |
|----------|-------------|----------|
| `simple` | Basic agent without tools | Q&A chatbots, summarizers |
| `basic-tools` | Simple agent with tool support | Single-purpose tool agents |
| `planner` | 3-phase architecture: Intent → Plan → Execute | Domain-specific agents (like clickup-agent) |
| `coordinator` | Multi-agent delegator pattern | Routing to specialist agents |

**Event Subscriptions:**

Agents can subscribe to events dispatched by the host (e.g., from task-daemon polling Slack, ClickUp, etc.).

Format: `--subscriptions "schema=<version>,sources=<kind1,kind2>"`

Example:
```bash
--subscriptions "schema=task-daemon.interpretation.v1,sources=slack,clickup"
```

This writes subscriptions to `manifest.json`:
```json
{
  "discovery": {
    "subscriptions": [
      {
        "schema_versions": ["task-daemon.interpretation.v1"],
        "source_kinds": ["slack", "clickup"]
      }
    ]
  }
}
```

**Interactive mode:** When using interactive mode, the CLI prompts:
1. "Does this agent need to receive events?" (Y/n)
2. If yes, select schema versions from known options
3. Select source kinds (auto-suggested from tools + common sources like slack, clickup, github_issues)

Use `list-event-sources` to see available source kinds and schema versions.

**Examples:**

```bash
# Interactive mode (recommended for new users)
cargo run -p cargo-agent-platform -- new-agent

# Preview what would be created (non-interactive)
cargo run -p cargo-agent-platform -- new-agent github-agent --dry-run

# Create a simple agent (no tools)
cargo run -p cargo-agent-platform -- new-agent qa-bot --description "Q&A assistant"

# Create an agent with tools using the planner template
cargo run -p cargo-agent-platform -- new-agent github-agent \
  --tools support/github,system/internal_a2a \
  --template planner \
  --description "GitHub issue and PR assistant"

# Create a coordinator agent (automatically includes system tools)
cargo run -p cargo-agent-platform -- new-agent my-coordinator \
  --template coordinator \
  --description "Routes requests to specialist agents"

# Create an agent that receives events from Slack and ClickUp
cargo run -p cargo-agent-platform -- new-agent intake-agent \
  --tools system/internal_a2a \
  --template planner \
  --subscriptions "schema=task-daemon.interpretation.v1,sources=slack,clickup" \
  --description "Processes events from Slack and ClickUp"
```

**What it creates:**

```
agents/<name>/
  manifest.json           # Agent metadata (name, version, tools, discovery)
  tsconfig.json           # TypeScript configuration
  baml_src/
    <name>_prompt.baml    # BAML prompt functions (template-specific)
    generated_tools.baml  # Auto-generated tool type interfaces
    planner.baml          # (coordinator template only) Workflow planning
  src/
    index.ts              # TypeScript entry point (template-specific)
    baml-runtime.d.ts     # Auto-generated TypeScript declarations
```

**Template Details:**

**`simple` / `basic-tools`:**
- Wraps the existing `run_bootstrap` from baml-rt-builder
- Generates a single BAML function and simple `index.ts`
- `basic-tools` is automatically selected when tools are specified with `simple`

**`planner` (3-phase architecture):**
- Based on the clickup-agent pattern
- Generates three BAML functions:
  - `Infer{Name}Intent` - Classifies user intent, asks for clarification, or rejects
  - `Plan{Name}Work` - Generates a step plan from validated intent
  - `Choose{Name}Action` - Executes one step at a time via `runGeneratedStepExecutor`
- TypeScript includes intent loop with `awaitInput` for clarification

**`coordinator` (multi-agent delegator):**
- Based on the coordinator-agent pattern
- Automatically includes tools: `system/discover_agents`, `system/discover_tools`, `system/internal_a2a`
- Generates workflow planning and synthesis BAML functions
- TypeScript includes DAG-based workflow execution with parallel node execution

**After creating an agent:**

1. Edit `src/index.ts` to customize your agent logic
2. Edit `baml_src/<name>_prompt.baml` to customize BAML prompts
3. Run `cargo run -p baml-rt-builder --bin baml-agent-builder` to package the agent

**Default LLM client in generated templates:**

Newly generated `simple`, `planner`, and `coordinator` templates use:

```baml
client DefaultClient {
  provider openai-generic
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}
```

You can change this in your agent's `baml_src/*.baml` files.

---

### `list-agents`

Lists all agent packages in `agents/` and `tests/fixtures/agents/` with their manifest metadata.

```bash
cargo run -p cargo-agent-platform -- list-agents
```

**Output includes:**
- Agent name
- Version
- Source (production or fixture)
- Description (truncated)

**Example output:**
```
NAME                         VERSION  SOURCE      DESCRIPTION
claude-session-demo          1.0.0    production  Development agent that turns natural-language requirement...
clickup-agent                1.0.0    production  Agent that interacts with ClickUp tasks and spaces
coordinator-agent            1.0.0    production  Coordinator agent that delegates to specialist sub-agents...

argument-chapman             1.0.0    fixture     Fixture: Monty Python argument sketch agent (responder)
conversational-context-auto  1.0.0    fixture     Fixture: provenance-backed automatic conversation context...

Total: 19 agent(s)
```

---

### `build`

Packages one or more agents into distributable tar.gz files. This wraps the `baml-agent-builder package` functionality with a more ergonomic CLI.

```bash
cargo run -p cargo-agent-platform -- build [names]... [options]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `[names]...` | Agent names (looks in `agents/` directory). Omit to build current directory. |

**Options:**

| Option | Description |
|--------|-------------|
| `--path <path>` | Explicit path to agent directory (overrides name lookup, only valid with single agent) |
| `-o, --output <dir>` | Output directory for tar.gz files (default: current directory) |

**Examples:**

```bash
# Build a single agent by name (looks in agents/)
cargo run -p cargo-agent-platform -- build github-agent

# Build multiple agents at once
cargo run -p cargo-agent-platform -- build clickup-agent notion-agent github-agent

# Build with explicit path (single agent only)
cargo run -p cargo-agent-platform -- build --path agents/clickup-agent

# Build from current directory (when inside an agent directory)
cd agents/my-agent && cargo run -p cargo-agent-platform -- build

# Build with custom output directory
cargo run -p cargo-agent-platform -- build github-agent -o /tmp/

# Build multiple agents to a specific directory
cargo run -p cargo-agent-platform -- build clickup-agent notion-agent -o ./dist/
```

**Example output:**
```
[1/4] Building agent 'github-agent'...
      Source: /path/to/agents/github-agent
      Output: /path/to/github-agent-1.0.0.tar.gz
[2/4] Copying BAML sources...
[3/4] Generating types and compiling TypeScript...

📝 Generating runtime type declarations...

⚙️  Compiling TypeScript...

📦 Packaging agent...
[4/4] Packaging complete.

Agent packaged successfully!

  Package: /path/to/github-agent-1.0.0.tar.gz
```

**What it does:**

1. Resolves the agent directory (by name, path, or current directory)
2. Reads `manifest.json` to get agent name and version
3. Copies BAML sources to build directory
4. Generates runtime type declarations (`baml-runtime.d.ts`)
5. Compiles TypeScript with type checking
6. Packages everything into a tar.gz archive

**Output naming:**

Each output file is named `<agent-name>-<version>.tar.gz`. By default, files are placed in the current working directory. Use `-o` to specify a different output directory.

**Running the packaged agent:**

After building, run the agent using `baml-agent-runner` directly. The runner must be built with appropriate features for the tools your agent uses:

```bash
# Build the runner with tool support (recommended for local dev)
cargo build -p baml-agent-runner --all-features --release

# Run the agent
./target/release/baml-agent-runner \
  clickup-agent-1.0.0.tar.gz \
  --serve-http 127.0.0.1:8080 \
  --provenance-db provenance.db
```

**Feature flags for baml-agent-runner:**

| Feature | Required for |
|---------|--------------|
| `http-tools` | ClickUp, Notion, Slack tools (`support/clickup`, `support/notion`, `support/slack`) |
| `memory` | Memory tools (`memory/add`, `memory/query`, etc.) |
| `llm-tests` | LLM-dependent tests (not needed for running agents) |

For local development, `--all-features` is the safest default to avoid "Unknown tool in manifest" errors when adding new tool crates.

**Runner options:**

| Option | Description |
|--------|-------------|
| `--serve-http <ADDR>` | Bind HTTP API on the given address (e.g., `127.0.0.1:8080`) |
| `--a2a-stdio` | Run A2A JSON-RPC loop over stdio |
| `--provenance-db <PATH>` | SQLite database path for provenance (default: `:memory:`) |
| `--web-dir <DIR>` | Directory with web UI assets to serve at root path |

**Typical workflow:**

```bash
# 1. Build the runner with features (once)
cargo build -p baml-agent-runner --features http-tools,memory --release

# 2. Build the agent
cargo agent-platform build clickup-agent

# 3. Run the agent
./target/release/baml-agent-runner \
  clickup-agent-1.0.0.tar.gz \
  --serve-http 127.0.0.1:8080
```

---

### `regen`

Regenerates `generated_tools.baml` and `baml-runtime.d.ts` for agents in both `agents/` and `tests/fixtures/agents/`.

```bash
cargo run -p cargo-agent-platform -- regen [names]...
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `[names]...` | Agent names to regenerate (omit for all agents) |

**Examples:**

```bash
# Regenerate all agents
cargo run -p cargo-agent-platform -- regen

# Regenerate a single agent
cargo run -p cargo-agent-platform -- regen clickup-agent

# Regenerate multiple specific agents
cargo run -p cargo-agent-platform -- regen clickup-agent notion-agent argument-chapman
```

**What it does:**

1. Scans `agents/` and `tests/fixtures/agents/` for directories containing `baml_src/`
2. If agent names are provided, filters to only those agents
3. For each agent:
   - Writes canonical `tsconfig.json`
   - Runs the `RuntimeTypeGenerator` to produce `src/baml-runtime.d.ts`
   - Syncs `generated_*.baml` files to the agent's `baml_src/` directory
   - Removes stale `generated_*.baml` files no longer emitted

**Example output (all agents):**
```
[regen] Regenerating 7 agents in agents...
  -> claude-session-demo... ok
  -> clickup-agent... ok
  -> coordinator-agent... ok
  ...
[regen] Regenerating 13 agents in fixtures...
  -> argument-chapman... ok
  -> conversational-context-auto... ok
  ...

Done! Regenerated 20 agent(s)
```

**Example output (specific agent):**
```
[regen] Regenerating 1 agent(s) in agents...
  -> clickup-agent... ok

Done! Regenerated 1 agent(s)
```

**When to run:**
- After adding or modifying tools
- After changing tool metadata (description, tags, etc.)
- After modifying BAML schemas that affect type generation
- Before committing changes that touch tool definitions

**CI usage:**

The `regen` command is equivalent to running:
```bash
cargo run -p baml-rt-builder --features http-tools,memory --bin regen_fixtures
```

---

## Generated Tool Template

When you run `new-tool`, the generated `lib.rs` includes:

- Input/Output structs with all required derives (`BamlType`, `JsonSchema`, `TS`, etc.)
- Error type with `From<Error> for BamlRtError` implementation
- Tool struct with `#[baml_tool(...)]` attribute
- `BamlTool` trait implementation with placeholder logic

The generated code follows the patterns established by existing tools like `calculator`, `clickup`, and `notion`.

---

## Troubleshooting

### "Tool already exists"

If you see this error, the tool crate directory already exists. Either:
- Choose a different name
- Delete the existing crate if it was a failed attempt

### Patches fail to apply

All patches are transactional - if any patch fails, all changes are rolled back. Common causes:
- TOML parsing errors in target files
- Missing expected sections (e.g., `[features]`)

Run `doctor` to verify workspace integrity after any manual edits.

### Tool not appearing in `list-tools`

Ensure:
1. The tool crate compiles (`cargo check -p baml-tools-<name>`)
2. The `force_link_all_tools!` macro includes the tool
3. The feature is enabled when building the CLI

Run `doctor` to diagnose missing linkage.

### Agent not appearing in `list-agents`

Ensure:
1. The agent directory contains a `manifest.json` file
2. The manifest is valid JSON with required fields (`name`, `version`, `entry_point`)
3. The agent is in either `agents/` or `tests/fixtures/agents/`

### `new-agent` fails with "Directory already exists"

The target directory must be empty or non-existent. Either:
- Choose a different name or output directory
- Delete the existing directory if it was a failed attempt

### `regen` fails for an agent

Common causes:
- Missing or invalid `baml_src/` directory
- BAML syntax errors in prompt files
- Missing `manifest.json`

Check the error message for the specific agent and fix the underlying issue.

---

## Agent Templates in Detail

### Planner Template Architecture

The planner template implements a 3-phase architecture inspired by the clickup-agent:

```
┌─────────────────────────────────────────────────────────────┐
│                    Phase 1: Intent                          │
│  Infer{Name}Intent(user_message) →                         │
│    NeedClarification | NotRelevant | {Name}Intent          │
│                                                             │
│  - Classifies user message                                  │
│  - Asks clarifying questions via awaitInput                 │
│  - Rejects irrelevant requests                              │
│  - Distills clean intent statement                          │
└─────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────┐
│                    Phase 2: Planning                        │
│  Plan{Name}Work(intent, operation_kind) → {Name}Plan       │
│                                                             │
│  - Generates step plan from validated intent                │
│  - Steps have kinds: navigate, execute, format              │
│  - No data fetching or execution in this phase              │
└─────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────┐
│                    Phase 3: Execution                       │
│  Choose{Name}Action(goal, step_description, ...) →         │
│    FinalResponse | {ToolName}SessionPlan                   │
│                                                             │
│  - Called via runGeneratedStepExecutor in a ReAct loop     │
│  - Executes one step at a time                              │
│  - Threads prior results forward to subsequent steps        │
└─────────────────────────────────────────────────────────────┘
```

### Coordinator Template Architecture

The coordinator template implements a multi-agent delegation pattern:

```
┌─────────────────────────────────────────────────────────────┐
│                    1. Discovery                             │
│  discoverAgents(userText) via system/discover_agents        │
│                                                             │
│  - Finds all available specialist agents                    │
│  - Filters out self (coordinator) and non-default instances │
└─────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────┐
│                    2. Planning                              │
│  PlanCoordinatorWorkflow(user_message, available_agents)   │
│    → WorkflowPlan { goal, nodes[], final_node_id }         │
│                                                             │
│  Node kinds:                                                │
│    - call_agent: Delegate to specialist via system/internal_a2a │
│    - foreach: Fan-out over items from upstream              │
│    - synthesize: Merge prior node outputs                   │
│    - clarify: Ask user for clarification                    │
│    - direct_answer: Respond without delegation              │
└─────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────┐
│                    3. Execution                             │
│  executeWorkflow(plan, artifacts)                           │
│                                                             │
│  - Topological sort for dependency ordering                 │
│  - Parallel execution with concurrency limit                │
│  - Clarify nodes suspend execution via awaitInput           │
└─────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────┐
│                    4. Synthesis                             │
│  SynthesizeCoordinatorResponse(user_message, transcript)   │
│    → CoordinatorAnswer { answer, goals, sources, ... }     │
│                                                             │
│  - Aggregates evidence from all workflow nodes              │
│  - Produces structured response with confidence score       │
└─────────────────────────────────────────────────────────────┘
```

---

## CI Integration

### Recommended CI Steps

```yaml
# 1. Check generated files are up to date
- name: Regenerate type declarations
  run: cargo run -p cargo-agent-platform -- regen

- name: Check for uncommitted changes
  run: |
    if ! git diff --quiet -- 'agents/' 'tests/fixtures/agents/'; then
      echo "::error::Generated files are stale. Run 'cargo agent-platform regen' and commit."
      exit 1
    fi

# 2. Validate workspace integrity
- name: Workspace integrity check
  run: cargo run -p cargo-agent-platform -- doctor --ci
```

### justfile Shortcuts

```just
# Canonical feature set for CI
ci-tool-features := "http-tools,memory"

# SDK CLI shortcuts
new-tool name bundle='support':
    cargo run -p cargo-agent-platform -- new-tool {{name}} --bundle {{bundle}}

new-agent name *args:
    cargo run -p cargo-agent-platform -- new-agent {{name}} {{args}}

regen:
    cargo run -p cargo-agent-platform -- regen

doctor:
    cargo run -p cargo-agent-platform -- doctor

doctor-ci:
    cargo run -p cargo-agent-platform -- doctor --ci

list-tools:
    cargo run -p cargo-agent-platform -- list-tools

list-agents:
    cargo run -p cargo-agent-platform -- list-agents

list-event-sources:
    cargo run -p cargo-agent-platform -- list-event-sources

build +names:
    cargo run -p cargo-agent-platform -- build {{names}}

# Run agents directly with baml-agent-runner (requires building with features)
run-agent *args:
    ./target/release/baml-agent-runner {{args}}
```

---

## See Also

- `sdk_plan.md` - Full implementation roadmap and design decisions
- `docs/host-to-agent-event-delivery.md` - Event delivery model and subscriptions
- `CLAUDE.md` - Agent platform architecture and conventions
- `agents/clickup-agent/` - Reference implementation for planner template
- `agents/coordinator-agent/` - Reference implementation for coordinator template
- `agents/workflow-intake-agent/` - Reference implementation for event-consuming agent
