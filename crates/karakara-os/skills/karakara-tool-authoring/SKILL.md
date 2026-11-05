---
name: karakara-tool-authoring
description: Build, test, and publish TypeScript tools for Karakara. Use when creating a tool outside the monorepo.
---

# Karakara tool authoring

## When to apply

- Writing a TypeScript tool with TypeBox schemas
- Declaring network or secret requests
- Publishing tools for an agent

## Workflow

1. Run `karak tool new --lang ts <name> --bundle <bundle>`.
2. Install dependencies with `npm install`.
3. Define TypeBox input and output schemas in `src/tool.ts`.
4. Add the handler with `defineTool`. Put outbound `host:port` entries in `net` and named secret requests in `secrets`.
5. Run `karak tool build`. It starts the generated MCP server and writes `tools.json` plus `tool-manifest.json`.
6. Run `karak tool test --input '<json>'`. This invokes the tool through the supervisor DevBackend.
7. Run `karak publish`. The CLI builds a `.ktool`, requests approval, and uploads it.
8. Add the published tool name to the agent and deploy it.

## Rules

- TypeBox is the source of truth for TypeScript types and JSON Schema. Do not maintain a second schema file.
- Use the fully qualified `bundle/tool` name in `defineTool`.
- Use `ctx.fetch`; list every destination in `net`. The SDK checks the list and the Sandbox host enforces it.
- Do not put secret values in source. Declare names in `secrets` and use `ctx.secrets.<NAME>`.
- Throw `ctx.error(class, message)` for classified failures.
- Return a value that matches the output schema. The SDK validates it before MCP returns it.

A plain server built with the official MCP SDK remains valid. Plain MCP projects
must provide toolset metadata and capabilities in `karakara.toml`.

## Commands

| Command | Purpose |
|---------|---------|
| `karak tool new --lang ts <name>` | Create a TypeBox `defineTool` project |
| `karak tool build [dir]` | Compile, call `tools/list`, and generate descriptor files |
| `karak tool test [dir] --input '<json>'` | Invoke through the DevBackend |
| `karak tool publish [dir]` | Build, approve, and ingest a `.ktool` |
| `karak publish` | Detect a tool project and use the same publish path |
