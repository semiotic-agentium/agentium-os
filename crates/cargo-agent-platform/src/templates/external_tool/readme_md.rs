//! Generator for the scaffolded `README.md`.
//!
//! Setup instructions come from the typed `Language` enum so adding a new
//! language requires a compile-error fix rather than a silent fall-through.

use baml_rt_tools::external_tools::{METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA};

use super::{InvocationMode, Language, Runtime, STARTER_INPUT_KEY, SandboxSource, ScaffoldContext};

pub fn generate(ctx: &ScaffoldContext<'_>) -> String {
    let tool_id = ctx.tool_id();
    let setup = Language::setup_block(ctx.language);

    // Keep the README's "supported methods" bullets in sync with the actual
    // `SUPPORTED_METHODS` list declared in `templates/mod.rs`.
    let supported_bullets = supported_methods_for(ctx)
        .into_iter()
        .map(|m| format!("- `{m}`"))
        .collect::<Vec<_>>()
        .join("\n");

    let manual_probe_section = manual_probe_section(ctx, &tool_id);
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

{supported_bullets}

## Local setup

```bash
{setup}
```

{manual_probe_section}

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
        supported_bullets = supported_bullets,
        manual_probe_section = manual_probe_section,
        bind_setup_section = bind_setup_section,
    )
}

fn manual_probe_section(ctx: &ScaffoldContext<'_>, tool_id: &str) -> String {
    let heading = if ctx.runtime == Runtime::Sandbox {
        "## Local probe (developer convenience)"
    } else {
        "## Manual probe"
    };

    let preface = if ctx.runtime == Runtime::Sandbox {
        "For sandbox runtime tools, the runner invokes `/tool-adapter` inside the sandbox.\n`tool-server` is **not** the runtime invoke path; it's only a local debugging helper.\n\n"
    } else {
        ""
    };

    match ctx.invocation_mode {
        InvocationMode::SingleShot => format!(
            r#"{heading}

{preface}```bash
printf '{{"jsonrpc":"2.0","id":1,"method":"{method_describe}","params":{{"tool_name":"{tool_id}"}}}}\n' | ./tool-server
printf '{{"jsonrpc":"2.0","id":2,"method":"{method_invoke}","params":{{"invocation_id":"demo","tool_name":"{tool_id}","input":{{"{input_key}":"hello"}}}}}}\n' | ./tool-server
```
"#,
            heading = heading,
            preface = preface,
            method_describe = METHOD_DESCRIBE,
            method_invoke = METHOD_INVOKE,
            tool_id = tool_id,
            input_key = STARTER_INPUT_KEY,
        ),
        InvocationMode::Session => format!(
            r#"{heading}

{preface}```bash
# open
printf '{{"jsonrpc":"2.0","id":1,"method":"tool/session_open","params":{{"invocation_id":"demo","tool_name":"{tool_id}","open_input":{{}}}}}}\n' | ./tool-server

# send input (replace demo-session with the session_id returned by session_open)
printf '{{"jsonrpc":"2.0","id":2,"method":"tool/session_send","params":{{"session_id":"demo-session","input":{{"{input_key}":"hello"}}}}}}\n' | ./tool-server

# read next step (payloadless, same session_id)
printf '{{"jsonrpc":"2.0","id":3,"method":"tool/session_read","params":{{"session_id":"demo-session"}}}}\n' | ./tool-server

# finish (same session_id)
printf '{{"jsonrpc":"2.0","id":4,"method":"tool/session_finish","params":{{"session_id":"demo-session"}}}}\n' | ./tool-server
```
"#,
            heading = heading,
            preface = preface,
            tool_id = tool_id,
            input_key = STARTER_INPUT_KEY,
        ),
    }
}

fn supported_methods_for(ctx: &ScaffoldContext<'_>) -> Vec<&'static str> {
    match ctx.invocation_mode {
        InvocationMode::SingleShot => vec![METHOD_DESCRIBE, METHOD_INVOKE],
        InvocationMode::Session => vec![
            METHOD_DESCRIBE,
            METHOD_SCHEMA,
            "tool/session_open",
            "tool/session_send",
            "tool/session_read",
            "tool/session_finish",
            "tool/session_abort",
        ],
    }
}

fn bind_setup_section(ctx: &ScaffoldContext<'_>) -> String {
    if !(ctx.runtime == Runtime::Sandbox && ctx.sandbox_source == Some(SandboxSource::Bind)) {
        return String::new();
    }

    let shared = r#"## Bind sandbox runtime notes

These notes apply to both bind modes:

- metadata starts with a portable tool-relative bind path and no source `runtime_digest`,
- host-resolved bind path + digest live in gitignored `tool-metadata.lock.json` after sync,
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

fn bind_setup_docker_mode(_ctx: &ScaffoldContext<'_>) -> String {
    r#"## Bind setup (Docker-assisted)

Helper scripts are scaffolded at:

- `./setup_bind_sandbox.sh` (build/export/sync/check)
- `./inspect_tool.py` (framed adapter probe: describe/schema/invoke)

```bash
./setup_bind_sandbox.sh --force
```

This script wraps `sandbox-bind-sync` to build `adapter/Dockerfile`, export bind
rootfs, write `tool-metadata.lock.json`, materialize adapter sidecar bundle
(`/etc/agentium/tool-bundle.json`), and run `check-external-tool`.

> Bind rootfs mode copies filesystem contents only. Docker image config
> (like `ENV TOOL_CMD=...`) is not guaranteed at runtime. The generated
> adapter resolves a default tool command without requiring env vars.

You can also call the command directly:

```bash
cargo run -q -p cargo-agent-platform -- sandbox-bind-sync \
  --tool-dir . \
  --image local-sandbox:latest \
  --force \
  --check
```

`adapter/tool-adapter` is a generated transport shim (TSRPC <-> raw stdio).
You usually only edit the scaffolded language source (`main.py`, `src/main.rs`, etc.).

Example probes:

```bash
# Reads bind artifact sidecar from .tmp/<tool>-rootfs by default.
./inspect_tool.py describe
./inspect_tool.py schema

# invoke requires a runnable adapter command
./inspect_tool.py invoke --input '{"message":"hello"}' -- docker run --rm -i local-sandbox:latest /tool-adapter
```
"#
    .to_string()
}

fn bind_setup_manual_mode() -> String {
    r#"## Bind setup (manual rootfs mode)

No Docker/setup script is generated in this mode.

Materialize bind rootfs externally, then sync the local runtime lock + digest:

```bash
ROOTFS=/abs/path/to/rootfs
cargo run -q -p cargo-agent-platform -- sandbox-bind-sync \
  --tool-dir . \
  --rootfs "$ROOTFS" \
  --check
```
"#
    .to_string()
}
