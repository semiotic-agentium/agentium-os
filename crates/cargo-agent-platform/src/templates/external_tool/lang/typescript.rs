// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_tools::external_tools::{
    ERR_INTERNAL, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR, METHOD_DESCRIBE, METHOD_INVOKE,
    PROTOCOL_VERSION, SUPPORTED_METHODS,
};

use super::super::{
    GeneratedFile, STARTER_INPUT_KEY, STARTER_OUTPUT_KEY, ScaffoldContext, tool_server_wrapper,
};

// ----- TypeScript toolchain pins -----
//
// Scaffolded projects copy these exact version specifiers. Bump them in this
// one place when the workspace's TS toolchain moves; the scaffolder is the
// single source of truth for starter dev-dependency versions.
const TYPESCRIPT_VERSION: &str = "^5.9.0";
const TYPES_NODE_VERSION: &str = "^22.0.0";

pub fn generate(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    let tool_id = ctx.tool_id();
    let tool_name = ctx.name;
    // Render SUPPORTED_METHODS as a TypeScript array literal.
    let supported_methods_ts = SUPPORTED_METHODS
        .iter()
        .map(|m| format!("{:?}", m))
        .collect::<Vec<_>>()
        .join(", ");

    let package_json = format!(
        r#"{{
  "name": "{tool_name}-external-tool",
  "version": "0.1.0",
  "private": true,
  "type": "commonjs",
  "scripts": {{
    "build": "tsc -p tsconfig.json",
    "start": "node dist/main.js"
  }},
  "devDependencies": {{
    "typescript": "{typescript_version}",
    "@types/node": "{types_node_version}"
  }}
}}
"#,
        typescript_version = TYPESCRIPT_VERSION,
        types_node_version = TYPES_NODE_VERSION,
    );

    let tsconfig_json = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "CommonJS",
    "moduleResolution": "node16",
    "outDir": "dist",
    "rootDir": "src",
    "strict": true,
    "esModuleInterop": true,
    "forceConsistentCasingInFileNames": true,
    "skipLibCheck": true,
    "types": ["node"]
  },
  "include": ["src/**/*.ts"]
}
"#;

    let main_ts = format!(
        r#"import {{ stdin, stdout, stderr }} from "node:process";

const PROTOCOL_VERSION = "{protocol_version}";
const METHOD_DESCRIBE = "{method_describe}";
const METHOD_INVOKE = "{method_invoke}";
// Matches the scaffold's `SUPPORTED_METHODS`. Describe response echoes this
// so a caller knows which methods the tool handles.
const SUPPORTED_METHODS: ReadonlyArray<string> = [{supported_methods_ts}];
const ERR_METHOD_NOT_FOUND = {err_method_not_found};
const ERR_PARSE_ERROR = {err_parse_error};
const ERR_INTERNAL = {err_internal};

// Starter-contract field names — schema in tool-metadata.json and this
// handler must agree, so both come from the scaffold generator.
const INPUT_KEY = "{input_key}";
const OUTPUT_KEY = "{output_key}";

type JsonRpcId = string | number | null;

type JsonRpcRequest = {{
  jsonrpc: "2.0";
  id: JsonRpcId;
  method: string;
  params?: any;
}};

type ErrorClass =
  | "configuration"
  | "invalid_argument"
  | "transient"
  | "permission"
  | "execution";

function writeResult(id: JsonRpcId, result: unknown): void {{
  stdout.write(JSON.stringify({{ jsonrpc: "2.0", id, result }}) + "\n");
}}

function writeError(
  id: JsonRpcId,
  code: number,
  message: string,
  errorClass: ErrorClass = "execution"
): void {{
  stdout.write(
    JSON.stringify({{
      jsonrpc: "2.0",
      id,
      error: {{
        code,
        message,
        data: {{ error_class: errorClass }}
      }}
    }}) + "\n"
  );
}}

function handleDescribe(id: JsonRpcId): void {{
  writeResult(id, {{
    protocol_version: PROTOCOL_VERSION,
    tool_name: "{tool_id}",
    supported_methods: SUPPORTED_METHODS
  }});
}}

function handleInvoke(id: JsonRpcId, req: JsonRpcRequest): void {{
  const input = req.params?.input ?? {{}};
  const raw = input[INPUT_KEY];
  const message = typeof raw === "string" ? raw : "";
  stderr.write(`invoke ${{INPUT_KEY}}=${{JSON.stringify(message)}}\n`);
  writeResult(id, {{ output: {{ [OUTPUT_KEY]: message }}, done: true }});
}}

async function main(): Promise<void> {{
  const chunks: Buffer[] = [];
  for await (const chunk of stdin) {{
    chunks.push(Buffer.from(chunk));
  }}

  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) {{
    writeError(null, ERR_PARSE_ERROR, "empty request", "invalid_argument");
    return;
  }}

  let req: JsonRpcRequest;
  try {{
    req = JSON.parse(raw);
  }} catch {{
    writeError(null, ERR_PARSE_ERROR, "invalid JSON", "invalid_argument");
    return;
  }}

  const id = req.id ?? null;

  if (req.method === METHOD_DESCRIBE) {{
    handleDescribe(id);
    return;
  }}

  if (req.method === METHOD_INVOKE) {{
    handleInvoke(id, req);
    return;
  }}

  writeError(id, ERR_METHOD_NOT_FOUND, `method not found: ${{req.method}}`, "invalid_argument");
}}

main().catch((err) => {{
  stderr.write(`fatal: ${{String(err)}}\n`);
  writeError(null, ERR_INTERNAL, "tool execution failure", "execution");
}});
"#,
        protocol_version = PROTOCOL_VERSION,
        method_describe = METHOD_DESCRIBE,
        method_invoke = METHOD_INVOKE,
        supported_methods_ts = supported_methods_ts,
        err_method_not_found = ERR_METHOD_NOT_FOUND,
        err_parse_error = ERR_PARSE_ERROR,
        err_internal = ERR_INTERNAL,
        input_key = STARTER_INPUT_KEY,
        output_key = STARTER_OUTPUT_KEY,
    );

    let gitignore = "node_modules/\ndist/\n";

    vec![
        GeneratedFile::new("package.json", package_json),
        GeneratedFile::new("tsconfig.json", tsconfig_json),
        GeneratedFile::new("src/main.ts", main_ts),
        GeneratedFile::new(".gitignore", gitignore),
        tool_server_wrapper("node \"$DIR/dist/main.js\""),
    ]
}
