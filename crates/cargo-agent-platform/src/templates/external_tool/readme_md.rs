//! Generator for the scaffolded `README.md`.
//!
//! Setup instructions come from the typed `Language` enum so adding a new
//! language requires a compile-error fix rather than a silent fall-through.

use baml_rt_tools::external_tools::{METHOD_DESCRIBE, METHOD_INVOKE, SUPPORTED_METHODS};

use super::{Language, STARTER_INPUT_KEY, ScaffoldContext};

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
"#,
        tool_id = tool_id,
        setup = setup,
        method_describe = METHOD_DESCRIBE,
        method_invoke = METHOD_INVOKE,
        input_key = STARTER_INPUT_KEY,
        supported_bullets = supported_bullets,
    )
}
