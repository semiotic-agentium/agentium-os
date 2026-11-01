// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! V2 TypeScript scaffold — the **blessed path** (`plans/external-tools-v2.md`
//! §3.1, `-ts-sdk.md`). A tool is one typed `defineTool` function; the
//! `@karakara/tool` SDK derives the descriptor and runs the wire. There is no
//! hand-written JSON-RPC framing, no `tool-manifest.json`, no `tool/schema`
//! response, and no `tool-server` wrapper — all of which the v1 scaffold
//! emitted and v2 deletes (no backwards-compatibility requirement).

use super::super::{GeneratedFile, tool_id as make_tool_id};

// Toolchain pins for scaffolded projects — bump here, the single source of
// truth for starter dependency versions.
const KARAKARA_TOOL_VERSION: &str = "^0.1.0";
const TYPESCRIPT_VERSION: &str = "^6.0.0";

pub fn generate(name: &str, bundle: &str, description: &str) -> Vec<GeneratedFile> {
    let tool_id = make_tool_id(bundle, name);
    let pkg_name = name;

    let package_json = format!(
        r#"{{
  "name": "{pkg_name}-tool",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": {{
    "build": "tsc -p tsconfig.json"
  }},
  "dependencies": {{
    "@karakara/tool": "{karakara_tool_version}"
  }},
  "devDependencies": {{
    "typescript": "{typescript_version}"
  }}
}}
"#,
        karakara_tool_version = KARAKARA_TOOL_VERSION,
        typescript_version = TYPESCRIPT_VERSION,
    );

    // Matches the agent builder's toolchain: bundler resolution, TS 6.
    let tsconfig_json = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ES2022"],
    "outDir": "dist",
    "rootDir": "src",
    "strict": true,
    "forceConsistentCasingInFileNames": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*.ts"]
}
"#;

    // The entire tool: one typed function. Starter is a trivial echo so a new
    // author can build + test immediately; swap the schemas and body for real
    // logic. Declare `net`/`secrets` only when the tool needs them.
    let tool_ts = format!(
        r#"import {{ defineTool, Type, type Static }} from "@karakara/tool";

// Input/output are the single source of truth: these TypeBox types produce
// both the static TS types AND the JSON Schema in the tool's descriptor.
const Input = Type.Object({{
  message: Type.String({{ description: "Text to echo back." }}),
}});
const Output = Type.Object({{
  echoed: Type.String(),
}});

export default defineTool(
  {{
    name: "{tool_id}",
    version: "0.1.0",
    description: {description:?},
    // read | write | irreversible — drives host disposition + idempotency.
    sideEffect: "read",
    input: Input,
    output: Output,
    // Declare outbound hosts the tool needs (resolved to a CIDR allow-grant):
    //   net: ["api.example.com:443"],
    // Declare secrets by name (value is injected by the host on egress; never
    // visible in this process):
    //   secrets: {{ API_KEY: "egress_bound" }},
  }},
  async (input: Static<typeof Input>, _ctx): Promise<Static<typeof Output>> => {{
    // Real tools call `ctx.fetch(...)` (restricted to declared hosts) and may
    // `throw ctx.error("transient", "...")` to classify failures.
    return {{ echoed: input.message }};
  }},
);
"#,
        tool_id = tool_id,
        description = description,
    );

    let readme_md = format!(
        r#"# {tool_id}

A Karakara external tool (v2), authored with `@karakara/tool`. The whole tool is
the typed function in `src/tool.ts`.

## Develop

```bash
npm install
karak tool test      # run under the DevBackend (same wire as prod, no microVM)
```

## Ship

```bash
karak publish        # derive the descriptor, bundle the JS, audit, store
```

The descriptor (schemas, side-effect, requested capabilities) is **derived** from
`src/tool.ts` at build time — there is no manifest or schema file to maintain.
"#,
        tool_id = tool_id,
    );

    let gitignore = "node_modules/\ndist/\ntool-descriptor.json\ntool.lock.json\n";

    vec![
        GeneratedFile::new("package.json", package_json),
        GeneratedFile::new("tsconfig.json", tsconfig_json),
        GeneratedFile::new("src/tool.ts", tool_ts),
        GeneratedFile::new("README.md", readme_md),
        GeneratedFile::new(".gitignore", gitignore),
    ]
}
