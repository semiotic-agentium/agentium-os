//! Generator for the scaffolded `README.md`.
//!
//! Setup instructions come from the typed `Language` enum so adding a new
//! language requires a compile-error fix rather than a silent fall-through.

use baml_rt_tools::external_tools::{METHOD_DESCRIBE, METHOD_INVOKE, SUPPORTED_METHODS};

use super::{Language, Runtime, STARTER_INPUT_KEY, SandboxSource, ScaffoldContext};

pub fn generate(ctx: &ScaffoldContext<'_>) -> String {
    let tool_id = ctx.tool_id();
    let setup = Language::setup_block(ctx.language);

    // Keep the README's "supported methods" bullets in sync with the actual
    // `SUPPORTED_METHODS` list declared in `templates/mod.rs`.
    let supported_bullets = SUPPORTED_METHODS
        .iter()
        .map(|m| format!("- `{m}`"))
        .collect::<Vec<_>>()
        .join("\n");

    let bind_setup_section = bind_setup_section(ctx);

    format!(
        r#"# {tool_id} external tool

This scaffold implements the Agent Platform external tool protocol (JSON-RPC over stdio).

## Wire contract

- Read one JSON-RPC request from stdin.
- Write one JSON-RPC response to stdout.
- Write logs/diagnostics to stderr only.
- Exit after one request.

Supported methods in the scaffold:

- `{method_describe}`
{supported_bullets}

## Local setup

```bash
{setup}
```

## Manual probe

```bash
printf '{{"jsonrpc":"2.0","id":1,"method":"{method_describe}","params":{{"tool_name":"{tool_id}"}}}}\n' | ./tool-server
printf '{{"jsonrpc":"2.0","id":2,"method":"{method_invoke}","params":{{"invocation_id":"demo","tool_name":"{tool_id}","input":{{"{input_key}":"hello"}}}}}}\n' | ./tool-server
```

## Using with runner dev mode

Set `BAML_EXTERNAL_TOOLS_DIR` to this tool directory (or a colon-separated list), then run your runner:

```bash
export BAML_EXTERNAL_TOOLS_DIR="$(pwd)"
```

Then reference this tool in an agent manifest as:

```json
{{ "name": "{tool_id}", "backend": "external" }}
```

{bind_setup_section}
"#,
        tool_id = tool_id,
        setup = setup,
        method_describe = METHOD_DESCRIBE,
        method_invoke = METHOD_INVOKE,
        input_key = STARTER_INPUT_KEY,
        supported_bullets = supported_bullets,
        bind_setup_section = bind_setup_section,
    )
}

fn bind_setup_section(ctx: &ScaffoldContext<'_>) -> String {
    if !(ctx.runtime == Runtime::Sandbox && ctx.sandbox_source == Some(SandboxSource::Bind)) {
        return String::new();
    }

    let shared = r#"## Bind sandbox runtime notes

These notes apply to both bind modes:

- metadata starts with a placeholder bind path and placeholder digest,
- real `runtime.image.path` + `runtime_digest` must be patched after rootfs exists,
- `check-external-tool` should pass before running the runner,
- sandbox adapters should support TSRPC-framed JSON-RPC for parity with sandbox execution.

"#;

    let mode_specific = if ctx.generate_docker {
        bind_setup_docker_mode(ctx)
    } else {
        bind_setup_manual_mode()
    };

    format!("{shared}{mode_specific}")
}

fn bind_setup_docker_mode(ctx: &ScaffoldContext<'_>) -> String {
    let rootfs_dir = format!("{}-{}-rootfs", ctx.bundle, ctx.name.replace('_', "-"));
    format!(
        r#"## Bind setup (Docker-assisted)

A helper script is scaffolded at `./setup_bind_sandbox.sh`.

```bash
./setup_bind_sandbox.sh --force
```

This script builds `adapter/Dockerfile`, exports bind rootfs, computes digest,
patches metadata, and runs `check-external-tool`.

A TSRPC inspector is also generated:

```bash
./inspect_tsrpc.py --adapter ./.tmp/{rootfs_dir}/tool-adapter describe
./inspect_tsrpc.py --adapter ./.tmp/{rootfs_dir}/tool-adapter invoke --message "hello"
```
"#,
        rootfs_dir = rootfs_dir,
    )
}

fn bind_setup_manual_mode() -> String {
    r#"## Bind setup (manual rootfs mode)

No Docker/setup script is generated in this mode.

Materialize bind rootfs externally, then patch metadata and validate:

```bash
ROOTFS=/abs/path/to/rootfs
DIGEST="$(cargo run -q -p cargo-agent-platform -- sandbox-digest --source bind "$ROOTFS")"
TMP_META="$(mktemp)"
jq --arg path "$ROOTFS" --arg digest "$DIGEST" '
  .runtime.image = {"kind":"bind","path":$path}
  | .runtime_digest = $digest
' ./tool-metadata.json > "$TMP_META" && mv "$TMP_META" ./tool-metadata.json
cargo run -q -p cargo-agent-platform -- check-external-tool --path .
```
"#
    .to_string()
}
