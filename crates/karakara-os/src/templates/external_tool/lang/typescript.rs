// SPDX-FileCopyrightText: 2026 Karakara Labs
// SPDX-License-Identifier: Apache-2.0

//! TypeScript scaffold for the TypeBox-backed `@karakara/tool` SDK.

use super::super::{GeneratedFile, tool_id as make_tool_id};

const TOOL_SDK_VERSION: &str = "^0.2.0";
const TYPESCRIPT_VERSION: &str = "^6.0.0";
const NODE_TYPES_VERSION: &str = "^22.0.0";

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
    "@karakara/tool": "{tool_sdk_version}"
  }},
  "devDependencies": {{
    "@types/node": "{node_types_version}",
    "typescript": "{typescript_version}"
  }}
}}
"#,
        tool_sdk_version = TOOL_SDK_VERSION,
        node_types_version = NODE_TYPES_VERSION,
        typescript_version = TYPESCRIPT_VERSION,
    );

    let tsconfig_json = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ES2022", "DOM"],
    "types": ["node"],
    "outDir": "dist",
    "rootDir": "src",
    "strict": true,
    "exactOptionalPropertyTypes": true,
    "noUncheckedIndexedAccess": true,
    "forceConsistentCasingInFileNames": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*.ts"]
}
"#;

    let tool_ts = format!(
        r#"import {{ defineTool, Type }} from "@karakara/tool";

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
    sideEffect: "read",
    input: Input,
    output: Output,
    net: [],
  }},
  async (input) => ({{ echoed: input.message }}),
);
"#,
        description = description,
    );

    let readme_md = format!(
        r#"# {tool_id}

This tool uses TypeBox schemas as its TypeScript types and JSON Schema contract.
`karak tool build` reads the SDK metadata over MCP and generates `tools.json`
and `tool-manifest.json`.

## Develop

```bash
npm install
karak tool test
```

## Ship

```bash
karak tool build
karak publish
```

Declare outbound hosts in `net` and named secrets in `secrets` inside
`defineTool`. The host reviews and enforces those requests.
"#,
    );

    let gitignore = "node_modules/\ndist/\ntools.json\ntool-manifest.json\ntool.lock.json\n";

    vec![
        GeneratedFile::new("package.json", package_json),
        GeneratedFile::new("tsconfig.json", tsconfig_json),
        GeneratedFile::new("src/tool.ts", tool_ts),
        GeneratedFile::new("README.md", readme_md),
        GeneratedFile::new(".gitignore", gitignore),
    ]
}
