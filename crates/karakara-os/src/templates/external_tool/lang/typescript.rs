// SPDX-FileCopyrightText: 2026 Karakara Labs
// SPDX-License-Identifier: Apache-2.0

//! TypeScript MCP scaffold. The generated server uses the official MCP SDK;
//! `@karakara/tool` only removes repeated registration boilerplate.

use super::super::{GeneratedFile, tool_id as make_tool_id};

const MCP_SDK_VERSION: &str = "^1.32.2";
const TYPESCRIPT_VERSION: &str = "^6.0.0";
const ZOD_VERSION: &str = "^3.25.0";

pub fn generate(name: &str, bundle: &str, description: &str) -> Vec<GeneratedFile> {
    let tool_id = make_tool_id(bundle, name);

    let package_json = format!(
        r#"{{
  "name": "{name}-toolset",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": {{
    "build": "tsc -p tsconfig.json"
  }},
  "dependencies": {{
    "@modelcontextprotocol/sdk": "{mcp_sdk_version}",
    "zod": "{zod_version}"
  }},
  "devDependencies": {{
    "typescript": "{typescript_version}"
  }}
}}
"#,
        mcp_sdk_version = MCP_SDK_VERSION,
        typescript_version = TYPESCRIPT_VERSION,
        zod_version = ZOD_VERSION,
    );

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

    let tool_ts = format!(
        r#"import {{ McpServer }} from "@modelcontextprotocol/sdk/server/mcp.js";
import {{ StdioServerTransport }} from "@modelcontextprotocol/sdk/server/stdio.js";
import {{ z }} from "zod";

const server = new McpServer({{ name: "{name}", version: "0.1.0" }});
server.registerTool(
  "{name}",
  {{
    description: {description:?},
    inputSchema: {{ message: z.string().describe("Text to echo back.") }},
    outputSchema: z.object({{ echoed: z.string() }}),
    annotations: {{ readOnlyHint: true, idempotentHint: true }},
  }},
  async (input) => {{
    const output = {{ echoed: input.message }};
    return {{
      content: [{{ type: "text", text: JSON.stringify(output) }}],
      structuredContent: output,
    }};
  }},
);

await server.connect(new StdioServerTransport());
"#,
        name = name,
        description = description,
    );

    let karakara_toml = format!(
        r#"[toolset]
name = "{bundle}"
version = "0.1.0"

# Requests apply to the whole MCP server process, not individual tools.
[toolset.capabilities]
net = []
secrets = []
"#,
    );

    let readme_md = format!(
        r#"# {tool_id}

This project is an MCP stdio server. `@karakara/tool` is optional sugar over
`@modelcontextprotocol/sdk`; a plain MCP server works too.

## Develop

```bash
npm install
karak tool test
```

## Ship

```bash
karak tool build   # asks the server for tools/list and writes tools.json
karak publish
```

Keep toolset-wide network and secret requests in `karakara.toml`. The host
reviews those requests separately from each tool's MCP interface.
"#,
    );

    let gitignore = "node_modules/\ndist/\ntools.json\ntool.lock.json\n";

    vec![
        GeneratedFile::new("package.json", package_json),
        GeneratedFile::new("tsconfig.json", tsconfig_json),
        GeneratedFile::new("src/tool.ts", tool_ts),
        GeneratedFile::new("karakara.toml", karakara_toml),
        GeneratedFile::new("README.md", readme_md),
        GeneratedFile::new(".gitignore", gitignore),
    ]
}
