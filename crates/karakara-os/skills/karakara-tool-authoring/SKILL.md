---
name: karakara-tool-authoring
description: Build, test, and publish MCP toolsets for Karakara. Use when creating a tool server outside the monorepo.
---

# Karakara tool authoring

## When to apply

- Creating an MCP stdio server
- Declaring toolset-wide network or secret requests
- Publishing tools for an agent

## Workflow

1. Run `karak tool new <name> --bundle <bundle>`.
2. Install dependencies with `npm install`.
3. Register tools with the official MCP SDK. The generated `defineTool` helper is optional.
4. Put toolset name, version, network requests, and secret requests in `karakara.toml`.
5. Run `karak tool build`. It starts the server, calls `tools/list`, and writes `tools.json`.
6. Run `karak tool test --input '<json>'`.
7. Run `karak publish`. The CLI builds a `.ktool`, requests approval, and uploads it.
8. Pin the published tool name and digest from the agent.

## Rules

- The MCP `tools/list` result is the interface contract. Do not maintain a second schema file.
- Network and secret requests apply to the server process. Keep them in `karakara.toml`, not in a tool handler.
- Do not read secret values in author code. The host applies approved bindings.
- Return `structuredContent` that matches the MCP output schema.

## Commands

| Command | Purpose |
|---------|---------|
| `karak tool new <name>` | Create a TypeScript MCP server project |
| `karak tool build [dir]` | Compile the server, call `tools/list`, and write `tools.json` |
| `karak tool test [dir] --input '<json>'` | Invoke the first tool through the DevBackend |
| `karak tool publish [dir]` | Build, approve, and ingest a `.ktool` |
| `karak publish` | Detect a tool project in the current directory and use the same publish path |
