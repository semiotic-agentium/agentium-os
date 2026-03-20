# Agentium OS SDK CLI

The `cargo-agent-platform` CLI automates scaffolding of new tools and agents, eliminating manual boilerplate while preserving the existing compile-time tool registration model.

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
- Access level (Read, Write, Delete, or None)

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

### `new-tool`

Creates a new tool crate with all necessary file patches for it to compile and be discoverable.

```bash
cargo run -p cargo-agent-platform -- new-tool <name> [options]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<name>` | Tool name in kebab-case (e.g., `github`, `jira`, `linear`) |

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--bundle <type>` | `support` | Bundle type. Currently only `support` is supported. |
| `--access <level>` | `read` | Access level: `read`, `write`, or `delete` |
| `--dry-run` | - | Preview changes without writing any files |

**Examples:**

```bash
# Preview what would be created
cargo run -p cargo-agent-platform -- new-tool github --dry-run

# Create a read-only tool
cargo run -p cargo-agent-platform -- new-tool github

# Create a tool with write access
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
| `crates/baml-rt-builder/Cargo.toml` | Adds feature forwarding |

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

---

## Future Commands (Phase 2)

The following commands are planned for Phase 2:

- `new-agent <name>` - Create a new agent package with templates
- `list-agents` - List all agent packages
- `regen` - Regenerate `generated_tools.baml` and `baml-runtime.d.ts`

See `sdk_cli_plan.md` for the full implementation roadmap.
